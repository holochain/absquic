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

enum DriverCmd {
    LocalAddr(tokio::sync::oneshot::Sender<AqResult<SocketAddr>>),
}

#[derive(Clone, Copy)]
enum Disposition {
    GotPending,
    MoreWork,
    #[allow(dead_code)]
    Stop,
}

impl Disposition {
    pub fn merge(&mut self, oth: Self) {
        use Disposition::*;
        *self = match (*self, oth) {
            (Stop, _) => Stop,
            (_, Stop) => Stop,
            (MoreWork, _) => MoreWork,
            (_, MoreWork) => MoreWork,
            (GotPending, GotPending) => GotPending,
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
    in_send: tokio::sync::mpsc::Sender<InUdpPacket>,
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
                // TODO TRACING ERROR and exit Ready
                panic!("{:?}", e);
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
        let mut read_bufs_raw = Vec::with_capacity(quinn_udp::BATCH_SIZE);
        for _ in 0..quinn_udp::BATCH_SIZE {
            let mut buf = bytes::BytesMut::with_capacity(max_udp_size);
            buf.resize(max_udp_size, 0);
            read_bufs_raw.push(buf);
        }

        let capacity = quinn_udp::BATCH_SIZE * 4;
        let (in_send, in_recv) = tokio::sync::mpsc::channel(capacity);
        let (packet_send, packet_recv) = tokio::sync::mpsc::channel(capacity);
        let (cmd_send, cmd_recv) = tokio::sync::mpsc::channel(capacity);

        (
            Self {
                state,
                socket,
                write_bufs: VecDeque::with_capacity(quinn_udp::BATCH_SIZE),
                read_bufs_raw: read_bufs_raw.into_boxed_slice(),
                read_bufs: VecDeque::with_capacity(quinn_udp::BATCH_SIZE),
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
        for _ in 0..32 {
            let mut disp = self.poll_cmd_recv(cx)?;
            disp.merge(self.poll_packet_recv(cx)?);
            disp.merge(self.poll_send(cx)?);
            disp.merge(self.poll_fwd_incoming(cx)?);
            disp.merge(self.poll_recv(cx)?);

            match disp {
                Disposition::Stop => return Ok(Poll::Ready(())),
                Disposition::GotPending => return Ok(Poll::Pending),
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
                Poll::Pending => return Ok(Disposition::GotPending),
                Poll::Ready(None) => {
                    // erm... this is never going to give results again
                    // but we don't want to keep asking for work because of it
                    // so, return GotPending even though we didn't : )
                    return Ok(Disposition::GotPending);
                }
                Poll::Ready(Some(t)) => match t {
                    DriverCmd::LocalAddr(resp) => {
                        let _ = resp.send(self.socket.local_addr().map_err(|e| format!("{:?}", e).into()));
                    }
                }
            }
        }
    }

    pub fn poll_packet_recv(
        &mut self,
        cx: &mut Context<'_>,
    ) -> AqResult<Disposition> {
        while self.write_bufs.len() < quinn_udp::BATCH_SIZE {
            match self.packet_recv.poll_recv(cx) {
                Poll::Pending => return Ok(Disposition::GotPending),
                Poll::Ready(None) => {
                    // erm... this is never going to give results again
                    // but we don't want to keep asking for work because of it
                    // so, return GotPending even though we didn't : )
                    return Ok(Disposition::GotPending);
                }
                Poll::Ready(Some(t)) => {
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
                            .map(|ecn| quinn_proto::EcnCodepoint::from_bits(ecn))
                            .flatten(),
                        contents: data,
                        segment_size,
                        src_ip,
                    });
                }
            }
        }
        Ok(Disposition::MoreWork)
    }

    pub fn poll_send(
        &mut self,
        _cx: &mut Context<'_>,
    ) -> AqResult<Disposition> {
        todo!()
    }

    pub fn poll_fwd_incoming(
        &mut self,
        _cx: &mut Context<'_>,
    ) -> AqResult<Disposition> {
        todo!()
    }

    pub fn poll_recv(
        &mut self,
        _cx: &mut Context<'_>,
    ) -> AqResult<Disposition> {
        todo!()
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
        quinn_udp::BATCH_SIZE
    }

    fn max_gso_segments(&self) -> usize {
        self.state.max_gso_segments()
    }

    fn send(&self, data: OutUdpPacket) -> AqBoxFut<'static, AqResult<()>> {
        let sender = self.packet_send.clone();
        Box::pin(async move {
            sender
                .send(data)
                .await
                .map_err(|_| "SocketClosed".into())
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

/*
use absquic_core::backend::*;
use absquic_core::deps::bytes;
use absquic_core::*;
use std::collections::VecDeque;
use std::net::SocketAddr;
use std::task::Poll;

struct QuinnUdpBackend {
    state: quinn_udp::UdpState,
    socket: quinn_udp::UdpSocket,
    write_bufs: VecDeque<quinn_proto::Transmit>,
    read_bufs: Box<[bytes::BytesMut]>,
}

impl UdpBackend for QuinnUdpBackend {
    fn local_addr(&self) -> AqResult<SocketAddr> {
        Ok(self.socket.local_addr()?)
    }

    fn batch_size(&self) -> usize {
        quinn_udp::BATCH_SIZE
    }

    fn max_gso_segments(&self) -> usize {
        self.state.max_gso_segments()
    }

    fn poll_send(
        &mut self,
        cx: &mut Context<'_>,
        data: &mut VecDeque<OutUdpPacket>,
    ) -> Poll<AqResult<()>> {
        while self.write_bufs.len() < quinn_udp::BATCH_SIZE && !data.is_empty()
        {
            let OutUdpPacket {
                dst_addr,
                ecn,
                data,
                segment_size,
                src_ip,
            } = data.pop_front().unwrap();
            self.write_bufs.push_back(quinn_proto::Transmit {
                destination: dst_addr,
                ecn: ecn
                    .map(|ecn| quinn_proto::EcnCodepoint::from_bits(ecn))
                    .flatten(),
                contents: data,
                segment_size,
                src_ip,
            });
        }

        if self.write_bufs.is_empty() {
            return Poll::Ready(Ok(()));
        }

        self.write_bufs.make_contiguous();

        let Self { state, socket, write_bufs, .. } = self;
        let count =
            match socket.poll_send(state, cx, write_bufs.as_slices().0) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Err(e)) => return Poll::Ready(Err(e.into())),
                Poll::Ready(Ok(count)) => count,
            };
        println!("wrote {}", count);
        write_bufs.drain(..count);
        Poll::Ready(Ok(()))
    }

    fn poll_recv(
        &mut self,
        cx: &mut Context<'_>,
        data: &mut VecDeque<InUdpPacket>,
    ) -> Poll<AqResult<()>> {
        let Self {
            socket, read_bufs, ..
        } = self;

        let count = read_bufs.len();
        let mut meta = vec![Default::default(); count];
        let mut bufs = read_bufs
            .iter_mut()
            .map(|b| std::io::IoSliceMut::new(b.as_mut()))
            .take(count)
            .collect::<Vec<_>>();

        match socket.poll_recv(cx, bufs.as_mut_slice(), meta.as_mut_slice()) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Err(ref e))
                if e.kind() == std::io::ErrorKind::ConnectionReset =>
            {
                // quinn ignores this error... poll again?
                Poll::Ready(Ok(()))
            }
            Poll::Ready(Err(e)) => Poll::Ready(Err(e.into())),
            Poll::Ready(Ok(msg_count)) => {
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
                    println!("read 1 {:?}", &packet.data[..8]);
                    data.push_back(packet);
                }
                Poll::Ready(Ok(()))
            }
        }
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
    fn bind(&self) -> AqBoxFut<'static, AqResult<Box<dyn UdpBackend>>> {
        let addr = self.addr;
        let max_udp_size = self.max_udp_size;
        Box::pin(async move {
            let socket = std::net::UdpSocket::bind(addr)?;
            let socket = quinn_udp::UdpSocket::from_std(socket)?;
            let mut read_bufs = Vec::with_capacity(quinn_udp::BATCH_SIZE);
            for _ in 0..quinn_udp::BATCH_SIZE {
                let mut buf = bytes::BytesMut::with_capacity(max_udp_size);
                buf.resize(max_udp_size, 0);
                read_bufs.push(buf);
            }
            let backend = QuinnUdpBackend {
                state: quinn_udp::UdpState::new(),
                socket,
                write_bufs: VecDeque::with_capacity(quinn_udp::BATCH_SIZE),
                read_bufs: read_bufs.into_boxed_slice(),
            };
            let backend: Box<dyn UdpBackend> = Box::new(backend);
            Ok(backend)
        })
    }
}
*/

#[cfg(test)]
mod test;
