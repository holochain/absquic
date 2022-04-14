//! Absquic quiche con

use crate::ep::*;
use crate::*;
use absquic_core::con::*;
use absquic_core::deps::{bytes, futures_core};
use absquic_core::rt::*;
use absquic_core::udp::*;
use absquic_core::*;
use std::collections::HashMap;
use std::collections::VecDeque;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::task::Context;
use std::task::Poll;

/// Absquic quiche con evt receiver
pub struct QuicheConRecv<R: Rt> {
    recv: BoxRecv<
        'static,
        (ConEvt, <<R as Rt>::Semaphore as Semaphore>::GuardTy),
    >,
}

impl<R: Rt> futures_core::stream::Stream for QuicheConRecv<R> {
    type Item = ConEvt;

    #[inline(always)]
    fn poll_next(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Self::Item>> {
        match self.recv.poll_recv(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Ready(Some((r, _))) => Poll::Ready(Some(r)),
        }
    }

    #[inline(always)]
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.recv.size_hint()
    }
}

pub(crate) fn quiche_con<R, U>(
    h3_config: Option<Arc<quiche::h3::Config>>,
    addr: SocketAddr,
    con: Pin<Box<quiche::Connection>>,
    udp_send: Arc<U>,
    ep_cmd_send: BoundMultiSend<R, EpCmd<R>>,
) -> (QuicheCon<R>, QuicheConRecv<R>, BoundMultiSend<R, ConCmd>)
where
    R: Rt,
    U: Udp,
{
    let (con_cmd_send, con_cmd_recv) = bound_channel(64);
    let g = con_cmd_send.try_acquire().unwrap();
    let _ = con_cmd_send.send(ConCmd::Tick, g);
    let (con_evt_send, con_evt_recv) = bound_channel::<R, _>(64);

    R::spawn(con_task::<R, U>(
        h3_config,
        con,
        udp_send,
        ep_cmd_send,
        con_cmd_send.clone(),
        con_evt_send,
        con_cmd_recv,
    ));

    let con = QuicheCon {
        addr,
        con_cmd_send: con_cmd_send.clone(),
    };

    let recv = QuicheConRecv { recv: con_evt_recv };

    (con, recv, con_cmd_send)
}

pub(crate) enum ConCmd {
    Tick,
    Timeout,
    InPak(UdpPak),
    Http3(Http3Msg, DynOnceSend<Result<()>>),
}

/// Absquic quiche con
pub struct QuicheCon<R>
where
    R: Rt,
{
    addr: SocketAddr,
    con_cmd_send: BoundMultiSend<R, ConCmd>,
}

impl<R> Con for QuicheCon<R>
where
    R: Rt,
{
    type AddrFut = BoxFut<'static, Result<SocketAddr>>;
    type Http3Fut = BoxFut<'static, Result<()>>;

    fn into_dyn(self) -> DynCon {
        DynCon(Arc::new(self))
    }

    fn addr(&self) -> Self::AddrFut {
        let addr = self.addr;
        BoxFut::new(async move { Ok(addr) })
    }

    fn http3(&self, item: Http3Msg) -> Self::Http3Fut {
        let con_cmd_send = self.con_cmd_send.clone();
        BoxFut::new(async move {
            let (s, r) = R::one_shot();
            let g = con_cmd_send.acquire().await;
            con_cmd_send.send(ConCmd::Http3(item, s), g)?;
            r.await.ok_or_else(|| other_err("ConShutdown"))?
        })
    }
}

struct H3OutboundProgress {
    /// if this is set, we still need to `send_response`
    pub reference: Option<u64>,
    pub headers: Option<Box<[(Box<[u8]>, Box<[u8]>)]>>,
    pub body: Vec<bytes::Bytes>,
    pub rsp: DynOnceSend<Result<()>>,
}

struct H3InboundProgress {
    pub headers: Vec<quiche::h3::Header>,
    pub body: Vec<bytes::Bytes>,
}

async fn con_task<R, U>(
    h3_config: Option<Arc<quiche::h3::Config>>,
    mut con: Pin<Box<quiche::Connection>>,
    udp_send: Arc<U>,
    _ep_cmd_send: BoundMultiSend<R, EpCmd<R>>,
    con_cmd_send: BoundMultiSend<R, ConCmd>,
    con_evt_send: BoundMultiSend<R, ConEvt>,
    mut con_cmd_recv: BoxRecv<
        'static,
        (ConCmd, <<R as Rt>::Semaphore as Semaphore>::GuardTy),
    >,
) where
    R: Rt,
    U: Udp,
{
    let mut send_buf = bytes::BytesMut::new();
    send_buf.resize(64 * 1024, 0);

    let mut evt_guard = None;
    let mut connected = false;

    #[allow(unused_variables)]
    let mut h3_con = None;

    let mut h3_outbound_wait: VecDeque<H3OutboundProgress> = VecDeque::new();
    let mut h3_outbound: HashMap<u64, H3OutboundProgress> = HashMap::new();
    let mut h3_inbound: HashMap<u64, H3InboundProgress> = HashMap::new();

    while let Some((cmd, cmd_guard)) = con_cmd_recv.recv().await {
        #[allow(unused_assignments)]
        let mut should_send = false;

        let mut cmd_guard = Some(cmd_guard);

        match cmd {
            ConCmd::Tick => should_send = true,
            ConCmd::Timeout => {
                tracing::debug!("process timeout");
                con.on_timeout();
                should_send = true;
            }
            ConCmd::InPak(mut pak) => {
                tracing::debug!("process in pak");
                let recv = quiche::RecvInfo { from: pak.addr };
                if let Err(err) = con.recv(&mut pak.data[..], recv) {
                    tracing::error!(?err);
                }
                should_send = true;
            }
            ConCmd::Http3(item, rsp) => {
                if item.reference.0 == u64::MAX {
                    // this is a reques
                    h3_outbound_wait.push_back(H3OutboundProgress {
                        reference: None,
                        headers: Some(item.headers),
                        body: item.body.into_vec(),
                        rsp,
                    });
                } else {
                    h3_outbound.insert(item.reference.0, H3OutboundProgress {
                        reference: Some(item.reference.0),
                        headers: Some(item.headers),
                        body: item.body.into_vec(),
                        rsp,
                    });
                }
            }
        }

        if let Some(timeout) = con.timeout() {
            tracing::debug!(?timeout, "sched timeout");
            let timeout = std::time::Instant::now() + timeout;
            let con_cmd_send = con_cmd_send.clone();
            let g = cmd_guard.take().unwrap();
            R::spawn(async move {
                R::sleep(timeout).await;
                let _ = con_cmd_send.send(ConCmd::Timeout, g);
            });
        }

        if evt_guard.is_none() {
            evt_guard = Some(con_evt_send.acquire().await);
        }

        tracing::trace!(connected = %con.is_established());
        if !connected && con.is_established() {
            tracing::debug!("connected");
            connected = true;

            #[allow(unused_assignments)]
            if let Some(h3_config) = &h3_config {
                h3_con = match quiche::h3::Connection::with_transport(
                    &mut con,
                    h3_config,
                ) {
                    Ok(h3_con) => {
                        tracing::info!("h3 connection established");
                        should_send = true;
                        Some(h3_con)
                    }
                    Err(err) => {
                        tracing::error!(?err);
                        None
                    }
                }
            }

            if let Err(_) = con_evt_send
                .send(ConEvt::Connected, evt_guard.take().unwrap())
            {
                tracing::warn!("con evt receiver not connected");
            }
        }

        if let Some(h3_con) = &mut h3_con {
            // establish new outgoing data
            while !h3_outbound_wait.is_empty() {
                let item = h3_outbound_wait.front().unwrap();
                let fin = item.is_empty();
                match h3_con.send_request(&mut con, &item.headers, fin) {
                    Err(quiche::h3::Error::StreamBlocked) => break,
                }
            }

            // process outgoing data already in progress

            // check for incoming http3
            loop {
                if evt_guard.is_none() {
                    evt_guard = Some(con_evt_send.acquire().await);
                }

                let mut read_datagram_flow_id = None;
                let mut read_body_stream_id = None;

                match h3_con.poll(&mut con) {
                    Err(quiche::h3::Error::Done) => break,
                    Err(err) => {
                        tracing::error!(?err);
                        return; // end the whole con_task
                    }
                    Ok((stream_id, evt)) => {
                        match evt {
                            quiche::h3::Event::Headers { list, has_body } => {
                                if has_body {
                                    h3_inbound.insert(stream_id, H3InboundProgress {
                                        headers: list,
                                        body: Vec::new(),
                                    });
                                } else {
                                    use quiche::h3::NameValue;
                                    let item = Http3Msg {
                                        headers: list.into_iter().map(|h| (
                                            h.name().to_vec().into_boxed_slice(),
                                            h.value().to_vec().into_boxed_slice(),
                                        )).collect::<Vec<_>>().into_boxed_slice(),
                                        body: vec![].into_boxed_slice(),
                                        reference: Opaque(stream_id),
                                    };
                                    if let Err(_) = con_evt_send.send(ConEvt::Http3(item), evt_guard.take().unwrap()) {
                                        return; // end the whole con_task
                                    }
                                }
                            }
                            quiche::h3::Event::Data => {
                                read_body_stream_id = Some(stream_id);
                            }
                            quiche::h3::Event::Finished => {
                                if let Some(item) = h3_inbound.remove(&stream_id) {
                                    use quiche::h3::NameValue;
                                    let item = Http3Msg {
                                        headers: item.headers.into_iter().map(|h| (
                                            h.name().to_vec().into_boxed_slice(),
                                            h.value().to_vec().into_boxed_slice(),
                                        )).collect::<Vec<_>>().into_boxed_slice(),
                                        body: item.body.into_boxed_slice(),
                                        reference: Opaque(stream_id),
                                    };
                                    if let Err(_) = con_evt_send.send(ConEvt::Http3(item), evt_guard.take().unwrap()) {
                                        return; // end the whole con_task
                                    }
                                }
                            }
                            quiche::h3::Event::Reset(_error_code) => {
                                // TODO - return error?
                                h3_inbound.remove(&stream_id);
                            }
                            quiche::h3::Event::Datagram => {
                                read_datagram_flow_id = Some(stream_id);
                            }
                            quiche::h3::Event::GoAway => {
                                tracing::warn!("GoAway received");
                                return; // end the whole con_task
                            }
                        }
                    }
                }

                if let Some(stream_id) = read_body_stream_id {
                    let mut item = h3_inbound.get_mut(&stream_id);
                    loop {
                        let len = {
                            match h3_con.recv_body(&mut con, stream_id, &mut send_buf[..]) {
                                Err(quiche::h3::Error::Done) => break,
                                Err(err) => {
                                    tracing::error!(?err);
                                    return; // end the whole con_task
                                }
                                Ok(len) => len,
                            }
                        };

                        if let Some(item) = &mut item {
                            item.body.push(bytes::Bytes::copy_from_slice(&send_buf[..len]));
                        }
                    }
                }

                if let Some(flow_id) = read_datagram_flow_id {
                    loop {
                        match h3_con.recv_dgram(&mut con, &mut send_buf[..]) {
                            Err(quiche::h3::Error::Done) => break,
                            Err(err) => {
                                tracing::error!(?err);
                                return; // end the whole con_task
                            }
                            Ok((_len, new_flow_id, _flow_id_len)) => {
                                if flow_id != new_flow_id {
                                    tracing::error!("bad api");
                                    return; // end the whole con_task
                                }
                                // otherwise we're just dropping the data
                                // we don't have an api yet to return it
                            }
                        }
                    }
                }
            }
        }

        if should_send {
            let max_len = con.max_send_udp_payload_size();
            if send_buf.len() < max_len {
                send_buf.resize(max_len, 0);
            }

            loop {
                match con.send(&mut send_buf[..]) {
                    Err(quiche::Error::Done) => {
                        break;
                    }
                    Err(err) => {
                        tracing::error!(?err);
                        return; // end the whole con_task
                    }
                    Ok((len, info)) => {
                        let pak = UdpPak {
                            addr: info.to,
                            data: send_buf[..len].to_vec(),
                            ip: None,
                            gso: None,
                            ecn: None,
                        };
                        tracing::trace!(
                            byte_count = %pak.data.len(),
                            in_ms = %(info.at.saturating_duration_since(std::time::Instant::now())).as_millis(),
                            "want udp send",
                        );
                        let udp_send = udp_send.clone();
                        // better way than spawning for every packet?
                        R::spawn(async move {
                            R::sleep(info.at).await;
                            if let Err(err) = udp_send.send(pak).await {
                                tracing::error!(?err);
                            }
                        });
                    }
                }
            }
        }
    }
}
