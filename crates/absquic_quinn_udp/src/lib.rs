#![deny(missing_docs)]
#![deny(warnings)]
#![deny(unsafe_code)]
//! absquic udp backend implementation backed by quinn-udp

use absquic_core::backend::*;
use absquic_core::deps::bytes;
use absquic_core::*;
use std::collections::VecDeque;
use std::net::SocketAddr;
use std::task::Context;
use std::task::Poll;

/// TODO - better way to do this??
/// WARNING - the OutUdpPacket struct must exactly match the Transmit struct
#[allow(unsafe_code)]
fn danger_horrible_transmute(
    input: &[OutUdpPacket],
) -> &[quinn_proto::Transmit] {
    unsafe { std::mem::transmute(input) }
}

struct QuinnUdpBackend {
    state: quinn_udp::UdpState,
    socket: quinn_udp::UdpSocket,
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
        let len = data.len();
        if len == 0 {
            return Poll::Ready(Ok(()));
        }
        data.make_contiguous();
        let Self { state, socket, .. } = self;
        let count = {
            let in_count = std::cmp::min(len, quinn_udp::BATCH_SIZE);
            let data =
                danger_horrible_transmute(&data.as_slices().0[0..in_count]);
            match socket.poll_send(state, cx, data) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Err(e)) => return Poll::Ready(Err(e.into())),
                Poll::Ready(Ok(count)) => count,
            }
        };
        data.drain(..count);
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
                    println!("@@@ - meta.len: {}", meta.len);
                    let buf = buf[0..meta.len].into();
                    let packet = InUdpPacket {
                        src_addr: meta.addr,
                        dst_ip: meta.dst_ip,
                        ecn: meta.ecn.map(|ecn| ecn as u8),
                        data: buf,
                    };
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
                read_bufs: read_bufs.into_boxed_slice(),
            };
            let backend: Box<dyn UdpBackend> = Box::new(backend);
            Ok(backend)
        })
    }
}

#[cfg(test)]
mod test;
