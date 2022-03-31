#![deny(missing_docs)]
#![deny(warnings)]
#![deny(unsafe_code)]
//! Absquic udp backend implementation backed by quinn-udp

use absquic_core::backend::*;
use absquic_core::deps::bytes;
use absquic_core::*;
use std::collections::VecDeque;
use std::sync::Arc;
use std::task::Context;
use std::task::Poll;

struct Front {
    Receiver<UdpBackendCmd>,
    Receiver<OutUdpPacket>,
    Sender<UdpBackendEvt>
}

impl Front {
    fn poll(
        &mut self,
        cx: &mut Context<'_>,
        tx: &mut VecDeque<quinn_proto::Transmit>,
        rx: &mut VecDeque<InUdpPacket>,
    ) -> Poll<AqResult<()>> {
    }
}

struct Back {
    state: Arc<quinn_udp::UdpState>,
    socket: quinn_udp::UdpSocket,
    read_bufs_raw: [bytes::BytesMut; quinn_udp::BATCH_SIZE],
    read_meta_raw: [quinn_udp::RecvMeta; quinn_udp::BATCH_SIZE],
}

impl Back {
    fn poll(
        &mut self,
        cx: &mut Context<'_>,
        tx: &mut VecDeque<quinn_proto::Transmit>,
        rx: &mut VecDeque<InUdpPacket>,
    ) -> Poll<AqResult<()>> {
        let Wrapper { state, socket, read_bufs_raw, read_meta_raw } = self;

        let mut did_work = false;

        if !tx.is_empty() {
            tx.make_contiguous();
            match socket.poll_send(&state, cx, tx.as_slices().0) {
                Poll::Pending => (),
                Poll::Ready(Err(e)) => {
                    return Poll::Ready(Err(e.into()))
                }
                Poll::Ready(Ok(n)) => {
                    did_work = true;
                    tx.drain(..n);
                }
            }
        }

        if rx.is_empty() {
            let mut bufs = read_bufs_raw
                .iter_mut()
                .map(|b| std::io::IoSliceMut::new(b.as_mut()))
                .collect::<Vec<_>>();
            match socket.poll_recv(
                cx,
                &mut bufs,
                read_meta_raw,
            )
            {
                Poll::Pending => (),
                Poll::Ready(Err(ref e))
                    if e.kind() == std::io::ErrorKind::ConnectionReset =>
                {
                    did_work = true;
                }
                Poll::Ready(Err(e)) => return Poll::Ready(Err(e.into())),
                Poll::Ready(Ok(msg_count)) => {
                    did_work = true;

                    if msg_count == 0 {
                        // this should hopefully be unreachable,
                        // but just in case, return the error
                        return Poll::Ready(Err("bad udp recv poll".into()));
                    }

                    for (meta, buf) in
                        read_meta_raw.iter().zip(read_bufs_raw.iter()).take(msg_count)
                    {
                        let buf = buf[0..meta.len].into();
                        let packet = InUdpPacket {
                            src_addr: meta.addr,
                            dst_ip: meta.dst_ip,
                            ecn: meta.ecn.map(|ecn| ecn as u8),
                            data: buf,
                        };
                        rx.push_back(packet);
                    }
                }
            }

        }

        if did_work {
            Poll::Ready(Ok(()))
        } else {
            Poll::Pending
        }
    }
}

/*
use absquic_core::backend::*;
use absquic_core::deps::{bytes, one_err};
use absquic_core::util::*;
use absquic_core::*;
use std::collections::VecDeque;
use std::future::Future;
use std::net::SocketAddr;
use std::sync::Arc;
use std::task::Context;
use std::task::Poll;

#[cfg(trace_timing)]
mod trace_timing {
    pub(crate) struct TraceTiming(std::time::Instant);

    impl Default for TraceTiming {
        fn default() -> Self {
            Self(std::time::Instant::now())
        }
    }

    impl Drop for TraceTiming {
        fn drop(&mut self) {
            let elapsed_us = self.0.elapsed().as_micros();
            tracing::trace!(%elapsed_us, "TraceTiming");
        }
    }
}

const BATCH_SIZE: usize = quinn_udp::BATCH_SIZE;

enum DriverCmd {
    LocalAddr(util::OneShotSender<SocketAddr>),
}

#[derive(Clone, Copy)]
enum Disposition {
    /// it's safe to return pending, i.e. we have wakers registered
    /// everywhere needed to ensure continued function of the driver
    PendOk,

    /// we need another poll loop to continue safely
    MoreWork,
}

impl Disposition {
    pub fn merge(&mut self, oth: Self) {
        use Disposition::*;
        *self = match (*self, oth) {
            (MoreWork, _) => MoreWork,
            (_, MoreWork) => MoreWork,
            (PendOk, PendOk) => PendOk,
        }
    }
}

#[allow(dead_code)]
struct Driver {
    state: Arc<quinn_udp::UdpState>,
    socket: quinn_udp::UdpSocket,
    write_bufs: VecDeque<quinn_proto::Transmit>,
    read_bufs_raw: Box<[bytes::BytesMut]>,
    read_bufs: VecDeque<InUdpPacket>,
    in_send: util::Sender<InUdpPacket>,
    packet_recv: util::Receiver<OutUdpPacket>,
    cmd_recv: util::Receiver<DriverCmd>,
}

impl Future for Driver {
    type Output = ();

    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Self::Output> {
        match self.poll_inner(cx) {
            Err(e) => {
                tracing::error!("{:?}", e);
                Poll::Ready(())
            }
            Ok(r) => r,
        }
    }
}

impl Driver {
    pub fn new(
        max_udp_size: usize,
        state: Arc<quinn_udp::UdpState>,
        socket: quinn_udp::UdpSocket,
    ) -> (
        Self,
        util::Receiver<InUdpPacket>,
        util::Sender<OutUdpPacket>,
        util::Sender<DriverCmd>,
    ) {
        tracing::debug!(
            batch_size = %BATCH_SIZE,
            initial_gso = %state.max_gso_segments(),
            %max_udp_size,
            "udp bind data"
        );

        let mut read_bufs_raw = Vec::with_capacity(BATCH_SIZE);
        for _ in 0..BATCH_SIZE {
            let mut buf = bytes::BytesMut::with_capacity(max_udp_size);
            buf.resize(max_udp_size, 0);
            read_bufs_raw.push(buf);
        }

        let capacity = (BATCH_SIZE * 4).max(16).min(64);
        let (in_send, in_recv) = util::channel(capacity);
        let (packet_send, packet_recv) = util::channel(capacity);
        let (cmd_send, cmd_recv) = util::channel(capacity);

        (
            Self {
                state,
                socket,
                write_bufs: VecDeque::with_capacity(BATCH_SIZE),
                read_bufs_raw: read_bufs_raw.into_boxed_slice(),
                read_bufs: VecDeque::with_capacity(BATCH_SIZE),
                in_send,
                packet_recv,
                cmd_recv,
            },
            in_recv,
            packet_send,
            cmd_send,
        )
    }

    pub fn poll_inner(&mut self, cx: &mut Context<'_>) -> AqResult<Poll<()>> {
        #[cfg(trace_timing)]
        let _tt = trace_timing::TraceTiming::default();

        for _ in 0..32 {
            // order matters significantly here
            // consider carefully before changing
            let mut disp = self.poll_cmd_recv(cx)?;
            disp.merge(self.poll_send(cx)?);
            disp.merge(self.poll_packet_recv(cx)?);
            disp.merge(self.poll_recv(cx)?);
            disp.merge(self.poll_fwd_incoming(cx)?);

            match disp {
                Disposition::PendOk => {
                    return Ok(Poll::Pending);
                }
                Disposition::MoreWork => (),
            }
        }

        // we're not done, but neither are we pending...
        // need to trigger the waker, and try again
        cx.waker().wake_by_ref();
        Ok(Poll::Pending)
    }

    pub fn poll_cmd_recv(
        &mut self,
        cx: &mut Context<'_>,
    ) -> AqResult<Disposition> {
        // these should be quick, loop until pending
        loop {
            match self.cmd_recv.poll_recv(cx) {
                Poll::Pending => {
                    // since this is the first poll function
                    // any internal state that might require polls
                    // of additional poll functions are irrelevant
                    // since they'll all come after us anyways
                    return Ok(Disposition::PendOk);
                }
                Poll::Ready(None) => {
                    return Err("incoming cmd channel closed".into());
                }
                Poll::Ready(Some(t)) => match t {
                    DriverCmd::LocalAddr(resp) => {
                        let _ = resp.send(
                            self.socket
                                .local_addr()
                                .map_err(|e| format!("{:?}", e).into()),
                        );
                    }
                },
            }
        }
    }

    pub fn poll_send(&mut self, cx: &mut Context<'_>) -> AqResult<Disposition> {
        let Driver {
            state,
            socket,
            write_bufs,
            ..
        } = self;

        while !write_bufs.is_empty() {
            write_bufs.make_contiguous();
            let count = std::cmp::min(BATCH_SIZE, write_bufs.len());

            match socket.poll_send(
                state,
                cx,
                &write_bufs.as_slices().0[0..count],
            ) {
                Poll::Pending => {
                    // the only relevant internal state that can change
                    // here is freeing up space for poll_packet_recv
                    // and that is called after us, so safe to PendOk here
                    return Ok(Disposition::PendOk);
                }
                Poll::Ready(Err(e)) => return Err(e.into()),
                Poll::Ready(Ok(count)) => {
                    write_bufs.drain(..count);
                }
            }
        }

        // return pending here because there is nothing to do
        // see notes in poll_packet_recv
        Ok(Disposition::PendOk)
    }

    pub fn poll_packet_recv(
        &mut self,
        cx: &mut Context<'_>,
    ) -> AqResult<Disposition> {
        if self.write_bufs.len() == BATCH_SIZE {
            // safe to return pending here because we know
            // the sender has already registered a pending
            // so we'll get woken when there's more space
            return Ok(Disposition::PendOk);
        }

        let mut did_work = false;

        while self.write_bufs.len() < BATCH_SIZE {
            match self.packet_recv.poll_recv(cx) {
                Poll::Pending => {
                    // since poll_send is called before us, if we did
                    // any work here, we need to re-run the loop
                    if did_work {
                        return Ok(Disposition::MoreWork);
                    } else {
                        return Ok(Disposition::PendOk);
                    }
                }
                Poll::Ready(None) => {
                    return Err("outgoing packet channel closed".into());
                }
                Poll::Ready(Some(t)) => {
                    did_work = true;
                    let OutUdpPacket {
                        dst_addr,
                        ecn,
                        data,
                        segment_size,
                        src_ip,
                    } = t;
                    self.write_bufs.push_back(quinn_proto::Transmit {
                        destination: dst_addr,
                        ecn: ecn
                            .map(|ecn| {
                                quinn_proto::EcnCodepoint::from_bits(ecn)
                            })
                            .flatten(),
                        contents: data,
                        segment_size,
                        src_ip,
                    });
                }
            }
        }

        // if we received a new packet, it means our buffer wasn't already
        // full at the top of this poll invocation.
        // recommend doing another round
        Ok(Disposition::MoreWork)
    }

    pub fn poll_recv(&mut self, cx: &mut Context<'_>) -> AqResult<Disposition> {
        let Driver {
            socket,
            read_bufs_raw,
            read_bufs,
            ..
        } = self;

        if !read_bufs.is_empty() {
            // safe to pend if we still have outgoing data because
            // poll_fwd_incoming below will either register a waker
            // on the outgoing channel or return MoreWork
            return Ok(Disposition::PendOk);
        }

        let count = read_bufs_raw.len();
        let mut meta = vec![Default::default(); count];
        let mut bufs = read_bufs_raw
            .iter_mut()
            .map(|b| std::io::IoSliceMut::new(b.as_mut()))
            .take(count)
            .collect::<Vec<_>>();

        loop {
            match socket.poll_recv(cx, bufs.as_mut_slice(), meta.as_mut_slice())
            {
                Poll::Pending => {
                    // poll fwd incoming is called after us
                    // so it's ok to return PendOk here
                    return Ok(Disposition::PendOk);
                }
                Poll::Ready(Err(ref e))
                    if e.kind() == std::io::ErrorKind::ConnectionReset =>
                {
                    // quinn ignores this error... polling again
                    continue;
                }
                Poll::Ready(Err(e)) => return Err(e.into()),
                Poll::Ready(Ok(msg_count)) => {
                    if msg_count == 0 {
                        // this should hopefully be unreachable,
                        // but just in case, return the error
                        return Err("bad udp recv poll".into());
                    }
                    for (meta, buf) in
                        meta.into_iter().zip(bufs.iter()).take(msg_count)
                    {
                        let buf = buf[0..meta.len].into();
                        let packet = InUdpPacket {
                            src_addr: meta.addr,
                            dst_ip: meta.dst_ip,
                            ecn: meta.ecn.map(|ecn| ecn as u8),
                            data: buf,
                        };
                        read_bufs.push_back(packet);
                    }
                    // safe to pend here for same reasons as above
                    return Ok(Disposition::PendOk);
                }
            }
        }
    }

    pub fn poll_fwd_incoming(
        &mut self,
        cx: &mut Context<'_>,
    ) -> AqResult<Disposition> {
        if self.read_bufs.is_empty() {
            // if poll_recv above didn't read any data, it always registers
            // a waker, so we need to return PendOk so we don't loop for
            // no reason
            return Ok(Disposition::PendOk);
        }

        while !self.read_bufs.is_empty() {
            match self.in_send.poll_send(cx) {
                Poll::Pending => {
                    // even if we cleared *some* of the items
                    // from this list, we don't need to re-run
                    // poll_recv until read_bufs is all the way empty
                    // so it's okay to return PendOk here
                    return Ok(Disposition::PendOk);
                }
                Poll::Ready(Err(e)) => return Err(e),
                Poll::Ready(Ok(send_cb)) => {
                    send_cb(self.read_bufs.pop_front().unwrap());
                }
            }
        }

        // we cleared our outgoing buffer and didn't register a waker
        // recommend polling again to see if we get more data
        Ok(Disposition::MoreWork)
    }
}

struct Sender {
    state: Arc<quinn_udp::UdpState>,
    packet_send: util::Sender<OutUdpPacket>,
    cmd_send: util::Sender<DriverCmd>,
}

impl UdpBackendSender for Sender {
    fn local_addr(&mut self) -> OneShotReceiver<SocketAddr> {
        let mut sender = self.cmd_send.clone();
        OneShotReceiver::new(async move {
            let (s, r) = util::one_shot_channel();
            sender
                .send()
                .await
                .map_err(|_| one_err::OneErr::new("SocketClosed"))?(
                DriverCmd::LocalAddr(s),
            );
            Ok(OneShotKind::Value(r.await?))
        })
    }

    fn batch_size(&self) -> usize {
        BATCH_SIZE
    }

    fn max_gso_segments(&self) -> usize {
        self.state.max_gso_segments()
    }

    fn poll_send(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<AqResult<SenderCb<OutUdpPacket>>> {
        self.packet_send.poll_send(cx)
    }
}

struct Receiver {
    receiver: util::Receiver<InUdpPacket>,
}

impl UdpBackendReceiver for Receiver {
    fn poll_recv(&mut self, cx: &mut Context<'_>) -> Poll<Option<InUdpPacket>> {
        self.receiver.poll_recv(cx)
    }
}

/// Absquic udp backend backed by the quinn-udp library
pub struct QuinnUdpBackendFactory {
    addr: SocketAddr,
    max_udp_size: usize,
}

impl QuinnUdpBackendFactory {
    /// Construct a new quinn udp backend factory
    pub fn new(addr: SocketAddr, max_udp_size: Option<usize>) -> Self {
        let max_udp_size = max_udp_size.unwrap_or_default();
        Self {
            addr,
            max_udp_size: std::cmp::max(64 * 1024, max_udp_size),
        }
    }
}

impl UdpBackendFactory for QuinnUdpBackendFactory {
    fn bind(
        &self,
    ) -> AqBoxFut<
        'static,
        AqResult<(DynUdpBackendSender, DynUdpBackendReceiver, BackendDriver)>,
    > {
        let addr = self.addr;
        let max_udp_size = self.max_udp_size;
        Box::pin(async move {
            let state = Arc::new(quinn_udp::UdpState::new());
            let socket = std::net::UdpSocket::bind(addr)?;
            let socket = quinn_udp::UdpSocket::from_std(socket)?;

            let addr = socket.local_addr();
            tracing::info!(?addr, "udp bound");

            let (driver, in_recv, packet_send, cmd_send) =
                Driver::new(max_udp_size, state.clone(), socket);

            let driver = BackendDriver::new(driver);

            let sender = Sender {
                state,
                packet_send,
                cmd_send,
            };

            let sender: DynUdpBackendSender = Box::new(sender);

            let receiver = Receiver { receiver: in_recv };

            let receiver: DynUdpBackendReceiver = Box::new(receiver);

            Ok((sender, receiver, driver))
        })
    }
}

#[cfg(all(test, not(loom)))]
mod test;
*/
