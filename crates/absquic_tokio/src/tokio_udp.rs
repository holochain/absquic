//! `feature = "tokio_udp"` Absquic_core Udp backed by tokio

use absquic_core::udp::*;
use absquic_core::deps::futures_core;
use absquic_core::*;
use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::atomic;
use std::sync::Arc;
use std::sync::Weak;
use std::task::Context;
use std::task::Poll;

struct ShutdownInner {
    waker: atomic_waker::AtomicWaker,
    shutdown: atomic::AtomicBool,
}

#[derive(Clone)]
struct Shutdown(Arc<ShutdownInner>);

impl Shutdown {
    pub fn new() -> Self {
        Self(Arc::new(ShutdownInner {
            waker: atomic_waker::AtomicWaker::new(),
            shutdown: atomic::AtomicBool::new(false),
        }))
    }

    pub fn shutdown(&self) {
        self.0.shutdown.store(true, atomic::Ordering::Relaxed);
        self.0.waker.wake();
    }

    pub fn is_shutdown(&self, cx: &mut Context<'_>) -> bool {
        if self.0.shutdown.load(atomic::Ordering::Relaxed) {
            return true;
        }
        self.0.waker.register(cx.waker());
        // AtomicWaker docs say we need to check after too
        if self.0.shutdown.load(atomic::Ordering::Relaxed) {
            return true;
        }
        false
    }
}

/// `feature = "tokio_udp"` Close immidiate return future type
pub struct TokioUdpCloseImmedFut;

impl Future for TokioUdpCloseImmedFut {
    type Output = ();

    #[inline(always)]
    fn poll(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
    ) -> Poll<Self::Output> {
        Poll::Ready(())
    }
}

/// `feature = "tokio_udp"` Addr return future type
pub struct TokioUdpAddrFut(Option<Result<SocketAddr>>);

impl Future for TokioUdpAddrFut {
    type Output = Result<SocketAddr>;

    #[inline(always)]
    fn poll(
        mut self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
    ) -> Poll<Self::Output> {
        Poll::Ready(self.0.take().unwrap())
    }
}

/// `feature = "tokio_udp"` Send return future type
pub struct TokioUdpSendFut(BoxFut<'static, Result<()>>);

impl TokioUdpSendFut {
    fn priv_new(
        sock: Arc<tokio::net::UdpSocket>,
        pak: UdpPak,
    ) -> Self {
        // just taking the boxing route for now, because it's much easier
        Self(BoxFut::new(async move {
            sock
                .send_to(&pak.data, pak.addr)
                .await
                .map(|_| ())
        }))
    }

    fn priv_new_err(err: std::io::Error) -> Self {
        Self(BoxFut::new(async move { Err(err) }))
    }
}

impl Future for TokioUdpSendFut {
    type Output = Result<()>;

    #[inline(always)]
    fn poll(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Self::Output> {
        Pin::new(&mut self.0).poll(cx)
    }
}

/// `feature = "tokio_udp"` Absquic_core Udp backed by tokio
pub struct TokioUdp {
    shutdown: Shutdown,
    sock: Weak<tokio::net::UdpSocket>,
}

impl TokioUdp {
    fn new(
        shutdown: Shutdown,
        sock: Weak<tokio::net::UdpSocket>,
    ) -> Self {
        Self { shutdown, sock }
    }
}

impl Udp for TokioUdp {
    type CloseImmedFut = TokioUdpCloseImmedFut;
    type AddrFut = TokioUdpAddrFut;
    type SendFut = TokioUdpSendFut;

    #[inline(always)]
    fn close_immediate(&self) -> Self::CloseImmedFut {
        self.shutdown.shutdown();
        TokioUdpCloseImmedFut
    }

    fn addr(&self) -> Self::AddrFut {
        TokioUdpAddrFut(Some(match Weak::upgrade(&self.sock) {
            Some(sock) => sock.local_addr(),
            None => Err(other_err("SocketClosed")),
        }))
    }

    fn send(&self, pak: UdpPak) -> Self::SendFut {
        match Weak::upgrade(&self.sock) {
            Some(sock) => TokioUdpSendFut::priv_new(sock, pak),
            None => TokioUdpSendFut::priv_new_err(other_err("SocketClosed")),
        }
    }
}

/// `feature = "tokio_udp"` Absquic_core Udp backed by tokio
pub struct TokioUdpRecv {
    shutdown: Shutdown,
    sock: Arc<tokio::net::UdpSocket>,
    buf: Box<[u8]>,
}

impl TokioUdpRecv {
    fn new(
        shutdown: Shutdown,
        sock: Arc<tokio::net::UdpSocket>,
        max_udp_size: usize,
    ) -> Self {
        let buf = vec![0; max_udp_size].into_boxed_slice();
        Self { shutdown, sock, buf }
    }
}

impl futures_core::Stream for TokioUdpRecv {
    type Item = Result<UdpPak>;

    fn poll_next(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Self::Item>> {
        if self.shutdown.is_shutdown(cx) {
            return Poll::Ready(None);
        }
        let Self { sock, buf, .. } = &mut *self;
        let mut buf = tokio::io::ReadBuf::new(&mut buf[..]);
        match sock.poll_recv_from(cx, &mut buf) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Err(err)) => {
                self.shutdown.shutdown();
                Poll::Ready(Some(Err(err.into())))
            }
            Poll::Ready(Ok(addr)) => {
                let data = buf.filled().to_vec();
                Poll::Ready(Some(Ok(UdpPak {
                    addr,
                    data,
                    ip: None,
                    gso: None,
                    ecn: None,
                })))
            }
        }
    }
}

/// `feature = "tokio_udp"` Absquic_core Udp backed by tokio
pub struct TokioUdpFactory {
    addr: SocketAddr,
    max_udp_size: usize,
}

impl TokioUdpFactory {
    /// Construct a new TokioUdpFactory set to bind to `addr`
    pub fn new(addr: SocketAddr, max_udp_size: Option<usize>) -> Self {
        let max_udp_size = std::cmp::max(64 * 1024, max_udp_size.unwrap_or(0));
        Self {
            addr,
            max_udp_size,
        }
    }
}

impl UdpFactory for TokioUdpFactory {
    type UdpTy = TokioUdp;
    type UdpRecvTy = TokioUdpRecv;
    type BindFut = BoxFut<'static, Result<(Self::UdpTy, Self::UdpRecvTy)>>;
    fn bind(self) -> Self::BindFut {
        let Self { addr, max_udp_size } = self;
        BoxFut::new(async move {
            let shutdown = Shutdown::new();
            let sock = Arc::new(tokio::net::UdpSocket::bind(addr).await?);
            let udp = TokioUdp::new(shutdown.clone(), Arc::downgrade(&sock));
            let udp_recv = TokioUdpRecv::new(shutdown, sock, max_udp_size);
            Ok((udp, udp_recv))
        })
    }
}
