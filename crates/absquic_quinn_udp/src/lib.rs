#![deny(missing_docs)]
#![deny(warnings)]
#![deny(unsafe_code)]
//! absquic udp backend implementation backed by quinn-udp

use absquic_core::backend::*;
use absquic_core::deps::{bytes, one_err};
use absquic_core::*;
use std::collections::VecDeque;
use std::future::Future;
use std::net::SocketAddr;
use std::sync::Arc;
use std::task::Context;
use std::task::Poll;

const BATCH_SIZE: usize = quinn_udp::BATCH_SIZE;

enum DriverCmd {
    LocalAddr(tokio::sync::oneshot::Sender<AqResult<SocketAddr>>),
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

type InSendCb = Box<dyn FnOnce(InUdpPacket) + 'static + Send>;

#[allow(dead_code)]
struct Driver {
    state: Arc<quinn_udp::UdpState>,
    socket: quinn_udp::UdpSocket,
    write_bufs: VecDeque<quinn_proto::Transmit>,
    read_bufs_raw: Box<[bytes::BytesMut]>,
    read_bufs: VecDeque<InUdpPacket>,
    in_send: tokio::sync::mpsc::Sender<InUdpPacket>,
    in_send_fut: Option<AqBoxFut<'static, AqResult<InSendCb>>>,
    packet_recv: tokio::sync::mpsc::Receiver<OutUdpPacket>,
    cmd_recv: tokio::sync::mpsc::Receiver<DriverCmd>,
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
        tokio::sync::mpsc::Receiver<InUdpPacket>,
        tokio::sync::mpsc::Sender<OutUdpPacket>,
        tokio::sync::mpsc::Sender<DriverCmd>,
    ) {
        let mut read_bufs_raw = Vec::with_capacity(BATCH_SIZE);
        for _ in 0..BATCH_SIZE {
            let mut buf = bytes::BytesMut::with_capacity(max_udp_size);
            buf.resize(max_udp_size, 0);
            read_bufs_raw.push(buf);
        }

        let capacity = (BATCH_SIZE * 4).max(16).min(64);
        let (in_send, in_recv) = tokio::sync::mpsc::channel(capacity);
        let (packet_send, packet_recv) = tokio::sync::mpsc::channel(capacity);
        let (cmd_send, cmd_recv) = tokio::sync::mpsc::channel(capacity);

        (
            Self {
                state,
                socket,
                write_bufs: VecDeque::with_capacity(BATCH_SIZE),
                read_bufs_raw: read_bufs_raw.into_boxed_slice(),
                read_bufs: VecDeque::with_capacity(BATCH_SIZE),
                in_send,
                in_send_fut: None,
                packet_recv,
                cmd_recv,
            },
            in_recv,
            packet_send,
            cmd_send,
        )
    }

    pub fn poll_inner(&mut self, cx: &mut Context<'_>) -> AqResult<Poll<()>> {
        for _ in 0..32 {
            // order matters significantly here
            // consider carefully before changing
            let mut disp = self.poll_cmd_recv(cx)?;
            disp.merge(self.poll_send(cx)?);
            disp.merge(self.poll_packet_recv(cx)?);
            disp.merge(self.poll_recv(cx)?);
            disp.merge(self.poll_fwd_incoming(cx)?);

            match disp {
                Disposition::PendOk => return Ok(Poll::Pending),
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
            // see if we have a permit future still dangling
            if let Some(send_cb) = {
                if let Some(mut fut) = self.in_send_fut.take() {
                    match std::pin::Pin::new(&mut fut).poll(cx) {
                        Poll::Pending => {
                            // even if we cleared *some* of the items
                            // from this list, we don't need to re-run
                            // poll_recv until read_bufs is all the way empty
                            // so it's okay to return PendOk here
                            self.in_send_fut = Some(fut);
                            return Ok(Disposition::PendOk);
                        }
                        Poll::Ready(Err(e)) => return Err(e),
                        Poll::Ready(Ok(send_cb)) => Some(send_cb),
                    }
                } else {
                    None
                }
            } {
                send_cb(self.read_bufs.pop_front().unwrap());
            }

            // optimization, use the try reserve
            while !self.read_bufs.is_empty() {
                if let Ok(permit) = self.in_send.try_reserve() {
                    permit.send(self.read_bufs.pop_front().unwrap());
                } else {
                    break;
                }
            }

            if self.read_bufs.is_empty() {
                // poll again to see if there's more incoming data
                return Ok(Disposition::MoreWork);
            }

            // try_reserve either errored or had no permits,
            // let's try the future route
            let fut = self.in_send.clone().reserve_owned();
            self.in_send_fut = Some(Box::pin(async move {
                let permit = fut
                    .await
                    .map_err(|e| one_err::OneErr::new(format!("{:?}", e)))?;
                let cb: InSendCb = Box::new(move |t| {
                    let _ = permit.send(t);
                });
                Ok(cb)
            }));
        }

        // we cleared our outgoing buffer and didn't register a waker
        // recommend polling again to see if we get more data
        Ok(Disposition::MoreWork)
    }
}

struct Sender {
    state: Arc<quinn_udp::UdpState>,
    packet_send: tokio::sync::mpsc::Sender<OutUdpPacket>,
    cmd_send: tokio::sync::mpsc::Sender<DriverCmd>,
}

impl UdpBackendSender for Sender {
    fn local_addr(&self) -> AqBoxFut<'static, AqResult<SocketAddr>> {
        let sender = self.cmd_send.clone();
        Box::pin(async move {
            let (s, r) = tokio::sync::oneshot::channel();
            sender
                .send(DriverCmd::LocalAddr(s))
                .await
                .map_err(|_| one_err::OneErr::new("SocketClosed"))?;
            r.await.map_err(|_| one_err::OneErr::new("SocketClosed"))?
        })
    }

    fn batch_size(&self) -> usize {
        BATCH_SIZE
    }

    fn max_gso_segments(&self) -> usize {
        self.state.max_gso_segments()
    }

    fn send(&self, data: OutUdpPacket) -> AqBoxFut<'static, AqResult<()>> {
        let sender = self.packet_send.clone();
        Box::pin(async move {
            sender.send(data).await.map_err(|_| "SocketClosed".into())
        })
    }
}

struct Receiver {
    receiver: tokio::sync::mpsc::Receiver<InUdpPacket>,
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
    /// construct a new quinn udp backend factory
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

            let (driver, in_recv, packet_send, cmd_send) =
                Driver::new(max_udp_size, state.clone(), socket);

            let driver = BackendDriver::new(driver);

            let sender = Sender {
                state,
                packet_send,
                cmd_send,
            };

            let sender: DynUdpBackendSender = Arc::new(sender);

            let receiver = Receiver { receiver: in_recv };

            let receiver: DynUdpBackendReceiver = Box::new(receiver);

            Ok((sender, receiver, driver))
        })
    }
}

#[cfg(test)]
mod test;
