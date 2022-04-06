#![deny(missing_docs)]
#![deny(warnings)]
#![deny(unsafe_code)]
//! Absquic udp backend implementation backed by quinn-udp

use absquic_core::backend::*;
use absquic_core::deps::bytes;
use absquic_core::deps::one_err;
use absquic_core::runtime::*;
use absquic_core::*;
use std::collections::VecDeque;
use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::task::Context;
use std::task::Poll;

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
    fn bind<Runtime: AsyncRuntime>(
        &self,
    ) -> AqFut<
        'static,
        AqResult<(
            MultiSender<UdpBackendCmd>,
            MultiSender<OutUdpPacket>,
            MultiReceiver<UdpBackendEvt>,
        )>,
    > {
        let addr = self.addr;
        let max_udp_size = self.max_udp_size;
        AqFut::new(async move {
            let chan_size = std::cmp::max(8, quinn_udp::BATCH_SIZE) * 2;

            let (cmd_send, cmd_recv) = Runtime::channel(chan_size);
            let (pak_in_send, pak_in_recv) = Runtime::channel(chan_size);
            let (evt_send, evt_recv) = Runtime::channel(chan_size);

            let socket = std::net::UdpSocket::bind(addr)?;
            let socket = quinn_udp::UdpSocket::from_std(socket)?;

            let addr = socket.local_addr();
            tracing::info!(?addr, "udp bound");

            let driver = Driver::new(
                socket,
                cmd_recv,
                pak_in_recv,
                max_udp_size,
                evt_send,
            );

            Runtime::spawn(driver);

            Ok((cmd_send, pak_in_send, evt_recv))
        })
    }
}

struct Driver {
    socket: quinn_udp::UdpSocket,
    cmd_handler: CmdHandler,
    packet_sender: PacketSender,
    packet_reader: PacketReader,
}

impl Future for Driver {
    type Output = AqResult<()>;

    fn poll(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Self::Output> {
        if let Err(err) = self.poll_inner(cx) {
            tracing::error!(?err);
            Poll::Ready(Err(err))
        } else {
            Poll::Pending
        }
    }
}

impl Driver {
    fn new(
        socket: quinn_udp::UdpSocket,
        cmd_recv: MultiReceiver<UdpBackendCmd>,
        pak_in_recv: MultiReceiver<OutUdpPacket>,
        max_udp_size: usize,
        evt_send: MultiSender<UdpBackendEvt>,
    ) -> Self {
        Self {
            socket,
            cmd_handler: CmdHandler::new(cmd_recv),
            packet_sender: PacketSender::new(pak_in_recv),
            packet_reader: PacketReader::new(max_udp_size, evt_send),
        }
    }

    fn poll_inner(&mut self, cx: &mut Context) -> AqResult<()> {
        let Driver {
            socket,
            cmd_handler,
            packet_sender,
            packet_reader,
        } = self;

        let mut more_work;

        let start = std::time::Instant::now();
        loop {
            more_work = false;

            let mut cmd_recv_want_close = false;
            cmd_handler.poll(
                cx,
                socket,
                &mut more_work,
                &mut cmd_recv_want_close,
            )?;

            let mut packet_sender_want_close = false;
            packet_sender.poll(
                cx,
                socket,
                &mut more_work,
                &mut packet_sender_want_close,
            )?;

            let mut packet_reader_want_close = false;
            packet_reader.poll(
                cx,
                socket,
                &mut more_work,
                &mut packet_reader_want_close,
            )?;

            if cmd_recv_want_close
                && packet_sender_want_close
                && packet_reader_want_close
            {
                return Err("QuinnUdpBackendDriverClosing".into());
            }

            if !more_work || start.elapsed().as_millis() >= 10 {
                break;
            }
        }

        tracing::trace!(elapsed_ms = %start.elapsed().as_millis(), "udp poll");

        if more_work {
            cx.waker().wake_by_ref();
        }

        Ok(())
    }
}

struct CmdHandler {
    cmd_recv: MultiReceiver<UdpBackendCmd>,
    cmd_recv_closed: bool,
}

impl CmdHandler {
    fn new(cmd_recv: MultiReceiver<UdpBackendCmd>) -> Self {
        Self {
            cmd_recv,
            cmd_recv_closed: false,
        }
    }

    fn poll(
        &mut self,
        cx: &mut Context<'_>,
        socket: &mut quinn_udp::UdpSocket,
        more_work: &mut bool,
        want_close: &mut bool,
    ) -> AqResult<()> {
        if self.cmd_recv_closed {
            *want_close = true;
            return Ok(());
        }

        for _ in 0..32 {
            match self.cmd_recv.poll_recv(cx) {
                Poll::Pending => return Ok(()),
                Poll::Ready(None) => {
                    tracing::debug!("cmd_recv closed");
                    self.cmd_recv_closed = true;
                    *want_close = true;
                    return Ok(());
                }
                Poll::Ready(Some(cmd)) => match cmd {
                    UdpBackendCmd::CloseImmediate => {
                        return Err("CloseImmediate".into());
                    }
                    UdpBackendCmd::GetLocalAddress(sender) => {
                        let addr =
                            socket.local_addr().map_err(one_err::OneErr::new);
                        tracing::trace!(?addr, "GetLocalAddress");
                        sender.send(addr);
                    }
                },
            }
        }

        // if we didn't return, there is more work
        *more_work = true;
        Ok(())
    }
}

struct PacketSender {
    state: Arc<quinn_udp::UdpState>,
    buffer: VecDeque<quinn_proto::Transmit>,
    pak_in_recv: MultiReceiver<OutUdpPacket>,
    pak_in_recv_closed: bool,
}

impl PacketSender {
    fn new(pak_in_recv: MultiReceiver<OutUdpPacket>) -> Self {
        let state = Arc::new(quinn_udp::UdpState::new());
        let buffer = VecDeque::with_capacity(quinn_udp::BATCH_SIZE);
        Self {
            state,
            buffer,
            pak_in_recv,
            pak_in_recv_closed: false,
        }
    }

    fn poll(
        &mut self,
        cx: &mut Context<'_>,
        socket: &mut quinn_udp::UdpSocket,
        more_work: &mut bool,
        want_close: &mut bool,
    ) -> AqResult<()> {
        while !self.pak_in_recv_closed
            && self.buffer.len() < quinn_udp::BATCH_SIZE
        {
            match self.pak_in_recv.poll_recv(cx) {
                Poll::Pending => break,
                Poll::Ready(None) => {
                    self.pak_in_recv_closed = true;
                    break;
                }
                Poll::Ready(Some(out)) => {
                    let OutUdpPacket {
                        dst_addr,
                        ecn,
                        data,
                        segment_size,
                        src_ip,
                    } = out;
                    self.buffer.push_back(quinn_proto::Transmit {
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

        if !self.buffer.is_empty() {
            self.buffer.make_contiguous();
            match socket.poll_send(
                &self.state,
                cx,
                self.buffer.as_slices().0,
            )? {
                Poll::Pending => (),
                Poll::Ready(n) => {
                    if n == 0 {
                        unreachable!(); // hopefuly
                    }

                    // if we were able to write something,
                    // we have space to receive more
                    *more_work = true;
                    self.buffer.drain(..n);
                }
            }
        }

        if self.pak_in_recv_closed && self.buffer.is_empty() {
            *want_close = true;
        }

        Ok(())
    }
}

struct PacketReader {
    evt_send: MultiSenderPoll<UdpBackendEvt>,
    evt_send_permits: VecDeque<OnceSender<UdpBackendEvt>>,
    evt_send_closed: bool,
    read_bufs_raw: Box<[bytes::BytesMut]>,
    read_meta_raw: Box<[quinn_udp::RecvMeta]>,
}

impl PacketReader {
    fn new(max_udp_size: usize, evt_send: MultiSender<UdpBackendEvt>) -> Self {
        let mut read_bufs_raw = Vec::with_capacity(quinn_udp::BATCH_SIZE);
        for _ in 0..quinn_udp::BATCH_SIZE {
            let mut buf = bytes::BytesMut::with_capacity(max_udp_size);
            buf.resize(max_udp_size, 0);
            read_bufs_raw.push(buf);
        }
        let read_bufs_raw = read_bufs_raw.into_boxed_slice();
        let read_meta_raw =
            vec![Default::default(); quinn_udp::BATCH_SIZE].into_boxed_slice();

        Self {
            evt_send: MultiSenderPoll::new(evt_send),
            evt_send_permits: VecDeque::with_capacity(quinn_udp::BATCH_SIZE),
            evt_send_closed: false,
            read_bufs_raw,
            read_meta_raw,
        }
    }

    fn poll(
        &mut self,
        cx: &mut Context<'_>,
        socket: &mut quinn_udp::UdpSocket,
        more_work: &mut bool,
        want_close: &mut bool,
    ) -> AqResult<()> {
        if self.evt_send_closed {
            *want_close = true;
            return Ok(());
        }

        while self.evt_send_permits.len() < quinn_udp::BATCH_SIZE {
            match self.evt_send.poll_acquire(cx) {
                Poll::Pending => return Ok(()),
                Poll::Ready(Err(_)) => {
                    self.evt_send_closed = true;
                    self.evt_send_permits.clear();
                    *want_close = true;
                    return Ok(());
                }
                Poll::Ready(Ok(permit)) => {
                    self.evt_send_permits.push_back(permit)
                }
            }
        }

        let mut bufs = self
            .read_bufs_raw
            .iter_mut()
            .map(|b| std::io::IoSliceMut::new(b.as_mut()))
            .collect::<Vec<_>>();

        match socket.poll_recv(cx, bufs.as_mut_slice(), &mut self.read_meta_raw)
        {
            Poll::Pending => (),
            Poll::Ready(Err(ref e))
                if e.kind() == std::io::ErrorKind::ConnectionReset =>
            {
                // quinn ignores this error... polling again
                *more_work = true;
            }
            Poll::Ready(Err(e)) => return Err(e.into()),
            Poll::Ready(Ok(msg_count)) => {
                *more_work = true;

                if msg_count == 0 {
                    unreachable!(); // hopefuly
                }

                for (meta, buf) in
                    self.read_meta_raw.iter().zip(bufs.iter()).take(msg_count)
                {
                    let buf = buf[0..meta.len].into();

                    let packet = InUdpPacket {
                        src_addr: meta.addr,
                        dst_ip: meta.dst_ip,
                        ecn: meta.ecn.map(|ecn| ecn as u8),
                        data: buf,
                    };
                    let packet = UdpBackendEvt::InUdpPacket(packet);

                    // we ensure the correct amount of send permits above
                    self.evt_send_permits.pop_front().unwrap().send(packet);
                }
            }
        }

        Ok(())
    }
}

#[cfg(all(test, not(loom)))]
mod test;
