//! `feature = "tokio_udp"` Absquic_core Udp backed by tokio

use crate::rt::*;
use crate::udp::*;
use crate::*;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::atomic;
use std::sync::Arc;
use std::task::Context;
use std::task::Poll;

enum SendCmd {
    CloseImmediate(DynOnceSend<()>),
    Addr(DynOnceSend<AqResult<SocketAddr>>),
    Pak(UdpPak, DynOnceSend<AqResult<()>>),
}

/// `feature = "tokio_udp"` Absquic_core Udp backed by tokio
pub struct TokioUdp<R: Rt> {
    shutdown: Arc<atomic::AtomicBool>,
    limit: Arc<R::Semaphore>,
    cmd_send:
        DynMultiSend<SendCmd, <<R as Rt>::Semaphore as Semaphore>::GuardTy>,
}

impl<R: Rt> TokioUdp<R> {
    fn new(sock: Arc<tokio::net::UdpSocket>) -> Self {
        let shutdown = Arc::new(atomic::AtomicBool::new(false));
        let limit = Arc::new(R::semaphore(64));
        let (cmd_send, cmd_recv) = R::channel();

        {
            let shutdown = shutdown.clone();
            let cmd_send = cmd_send.clone();
            R::spawn(send_task::<R>(sock, shutdown, cmd_send, cmd_recv));
        }

        Self {
            shutdown,
            limit,
            cmd_send,
        }
    }
}

async fn send_task<R: Rt>(
    sock: Arc<tokio::net::UdpSocket>,
    _shutdown: Arc<atomic::AtomicBool>,
    cmd_send: DynMultiSend<
        SendCmd,
        <<R as Rt>::Semaphore as Semaphore>::GuardTy,
    >,
    mut cmd_recv: DynMultiRecv<
        SendCmd,
        <<R as Rt>::Semaphore as Semaphore>::GuardTy,
    >,
) {
    while let Some((evt, g)) = stream_recv(Pin::new(&mut cmd_recv)).await {
        match evt {
            SendCmd::CloseImmediate(rsp) => {
                rsp(());
                break;
            }
            SendCmd::Addr(rsp) => {
                rsp(sock.local_addr().map_err(one_err::OneErr::new));
            }
            SendCmd::Pak(mut pak, rsp) => {
                if let Some(at) = pak.at.take() {
                    let now = std::time::Instant::now();
                    if at > now {
                        let cmd_send = cmd_send.clone();
                        R::spawn(async move {
                            R::sleep(at).await;
                            let _ = cmd_send.send(SendCmd::Pak(pak, rsp), g);
                        });
                        continue;
                    }
                }
                let res = match sock.send_to(&pak.data, pak.addr).await {
                    Ok(_) => Ok(()),
                    Err(e) => Err(one_err::OneErr::new(e)),
                };
                rsp(res);
            }
        }
    }
}

impl<R: Rt> Udp for TokioUdp<R> {
    type CloseImmedFut = AqFut<'static, ()>;
    type AddrFut = AqFut<'static, AqResult<SocketAddr>>;
    type SendFut = AqFut<'static, AqResult<()>>;

    fn close_immediate(&self) -> Self::CloseImmedFut {
        let (s, r) = R::one_shot();
        self.shutdown.store(true, atomic::Ordering::Release);
        let limit = self.limit.clone();
        let cmd_send = self.cmd_send.clone();
        AqFut::new(async move {
            let g = limit.acquire().await;
            let _ = cmd_send.send(SendCmd::CloseImmediate(s), g);
            let _ = r.await;
        })
    }

    fn addr(&self) -> Self::AddrFut {
        let (s, r) = R::one_shot::<AqResult<SocketAddr>>();
        let limit = self.limit.clone();
        let cmd_send = self.cmd_send.clone();
        AqFut::new(async move {
            let g = limit.acquire().await;
            cmd_send.send(SendCmd::Addr(s), g)?;
            r.await.ok_or(ChannelClosed)?
        })
    }

    fn send(&self, pak: UdpPak) -> Self::SendFut {
        let (s, r) = R::one_shot::<AqResult<()>>();
        let limit = self.limit.clone();
        let cmd_send = self.cmd_send.clone();
        AqFut::new(async move {
            let g = limit.acquire().await;
            cmd_send.send(SendCmd::Pak(pak, s), g)?;
            r.await.ok_or(ChannelClosed)?
        })
    }
}

/// yo
pub struct TokioUdpRecv<R: Rt> {
    pak_recv: DynMultiRecv<
        AqResult<UdpPak>,
        <<R as Rt>::Semaphore as Semaphore>::GuardTy,
    >,
}

impl<R: Rt> TokioUdpRecv<R> {
    fn new(_sock: Arc<tokio::net::UdpSocket>) -> Self {
        let (_pak_send, pak_recv) = R::channel();
        Self { pak_recv }
    }
}

impl<R: Rt> futures_core::Stream for TokioUdpRecv<R> {
    type Item = AqResult<UdpPak>;

    fn poll_next(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Self::Item>> {
        match Pin::new(&mut self.pak_recv).poll_next(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Ready(Some((t, _))) => Poll::Ready(Some(t)),
        }
    }
}

/// `feature = "tokio_udp"` Absquic_core Udp backed by tokio
pub struct TokioUdpFactory<R: Rt> {
    addr: SocketAddr,
    _p: std::marker::PhantomData<R>,
}

impl<R: Rt> TokioUdpFactory<R> {
    /// Construct a new TokioUdpFactory set to bind to `addr`
    pub fn new(addr: SocketAddr) -> Self {
        Self {
            addr,
            _p: std::marker::PhantomData,
        }
    }
}

impl<R: Rt> UdpFactory for TokioUdpFactory<R> {
    type UdpTy = TokioUdp<R>;
    type UdpRecvTy = TokioUdpRecv<R>;
    type BindFut = AqFut<'static, AqResult<(Self::UdpTy, Self::UdpRecvTy)>>;
    fn bind(self) -> Self::BindFut {
        let addr = self.addr;
        AqFut::new(async move {
            let sock = Arc::new(tokio::net::UdpSocket::bind(addr).await?);
            let udp = TokioUdp::new(sock.clone());
            let udp_recv = TokioUdpRecv::new(sock);
            Ok((udp, udp_recv))
        })
    }
}
