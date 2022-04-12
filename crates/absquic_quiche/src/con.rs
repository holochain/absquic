//! Absquic quiche con

use absquic_core::con::*;
use absquic_core::deps::futures_core;
use absquic_core::rt::*;
use absquic_core::*;
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

pub(crate) type ConCmdSend<R> =
    DynMultiSend<(ConCmd, <<R as Rt>::Semaphore as Semaphore>::GuardTy)>;

pub(crate) fn quiche_con<R>(
    addr: SocketAddr,
) -> (QuicheCon<R>, QuicheConRecv<R>, ConCmdSend<R>)
where
    R: Rt,
{
    let (con_cmd_send, con_cmd_recv) =
        R::channel::<(ConCmd, <<R as Rt>::Semaphore as Semaphore>::GuardTy)>();
    let (_con_evt_send, con_evt_recv) = R::channel();

    R::spawn(con_task::<R>(con_cmd_recv));

    let con = QuicheCon {
        addr,
        con_cmd_send: con_cmd_send.clone(),
    };

    let recv = QuicheConRecv { recv: con_evt_recv };

    (con, recv, con_cmd_send)
}

pub(crate) enum ConCmd {}

/// Absquic quiche con
pub struct QuicheCon<R>
where
    R: Rt,
{
    addr: SocketAddr,
    #[allow(dead_code)]
    con_cmd_send: ConCmdSend<R>,
}

impl<R> Con for QuicheCon<R>
where
    R: Rt,
{
    type AddrFut = BoxFut<'static, Result<SocketAddr>>;

    fn into_dyn(self) -> DynCon {
        DynCon(Arc::new(self))
    }

    fn addr(&self) -> Self::AddrFut {
        let addr = self.addr;
        BoxFut::new(async move { Ok(addr) })
    }
}

async fn con_task<R>(
    mut con_cmd_recv: BoxRecv<
        'static,
        (ConCmd, <<R as Rt>::Semaphore as Semaphore>::GuardTy),
    >,
) where
    R: Rt,
{
    while let Some((_cmd, _g)) = con_cmd_recv.recv().await {}
}
