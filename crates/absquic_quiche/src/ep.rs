//! Absquic quiche ep

use crate::con::*;
use absquic_core::deps::futures_core;
use absquic_core::ep::*;
use absquic_core::rt::*;
use absquic_core::udp::*;
use absquic_core::*;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::atomic;
use std::sync::Arc;
use std::task::Context;
use std::task::Poll;

/// Absquic quiche ep evt recevier
pub struct QuicheEpRecv<R: Rt> {
    recv: BoxRecv<
        'static,
        (
            EpEvt<QuicheCon<R>, QuicheConRecv<R>>,
            <<R as Rt>::Semaphore as Semaphore>::GuardTy,
        ),
    >,
}

impl<R: Rt> futures_core::stream::Stream for QuicheEpRecv<R> {
    type Item = EpEvt<QuicheCon<R>, QuicheConRecv<R>>;

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

pub(crate) fn quiche_ep<R, U, URecv>(
    config: quiche::Config,
    udp_send: U,
    udp_recv: URecv,
) -> (QuicheEp<R, U>, QuicheEpRecv<R>)
where
    R: Rt,
    U: Udp,
    URecv: futures_core::Stream<Item = Result<UdpPak>> + 'static + Send + Unpin,
{
    let udp_send = Arc::new(udp_send);

    let (ep_cmd_send, ep_cmd_recv) =
        R::channel::<(EpCmd<R>, <<R as Rt>::Semaphore as Semaphore>::GuardTy)>(
        );
    let (_ep_evt_send, ep_evt_recv) = R::channel();

    R::spawn(udp_task::<R, _>(udp_recv, ep_cmd_send.clone()));
    R::spawn(ep_task::<R, _>(config, udp_send.clone(), ep_cmd_recv));

    let limit = R::semaphore(64);
    let ep = QuicheEp {
        limit,
        udp_send,
        ep_cmd_send,
    };

    let recv = QuicheEpRecv { recv: ep_evt_recv };

    (ep, recv)
}

enum EpCmd<R: Rt> {
    InPak(Option<Result<UdpPak>>),
    NewCon(
        SocketAddr,
        DynOnceSend<Result<(QuicheCon<R>, QuicheConRecv<R>)>>,
    ),
}

/// Absquic quiche ep
pub struct QuicheEp<R, U>
where
    R: Rt,
    U: Udp,
{
    limit: R::Semaphore,
    udp_send: Arc<U>,
    ep_cmd_send:
        DynMultiSend<(EpCmd<R>, <<R as Rt>::Semaphore as Semaphore>::GuardTy)>,
}

impl<R, U> Ep for QuicheEp<R, U>
where
    R: Rt,
    U: Udp,
{
    type ConTy = QuicheCon<R>;
    type ConRecvTy = QuicheConRecv<R>;
    type AddrFut = U::AddrFut;
    type ConFut = BoxFut<'static, Result<(Self::ConTy, Self::ConRecvTy)>>;

    fn into_dyn(self) -> DynEp {
        DynEp(Arc::new(self))
    }

    fn addr(&self) -> Self::AddrFut {
        self.udp_send.addr()
    }

    fn connect(&self, addr: SocketAddr) -> Self::ConFut {
        let (s, r) = R::one_shot();
        let guard_fut = self.limit.acquire();
        let ep_cmd_send = self.ep_cmd_send.clone();
        BoxFut::new(async move {
            let g = guard_fut.await;
            ep_cmd_send.send((EpCmd::NewCon(addr, s), g))?;
            r.await.ok_or_else(|| other_err("EndpointClosed"))?
        })
    }
}

async fn udp_task<R, URecv>(
    mut udp_recv: URecv,
    ep_cmd_send: DynMultiSend<(
        EpCmd<R>,
        <<R as Rt>::Semaphore as Semaphore>::GuardTy,
    )>,
) where
    R: Rt,
    URecv: futures_core::Stream<Item = Result<UdpPak>> + 'static + Send + Unpin,
{
    let limit = R::semaphore(64);
    loop {
        let g = limit.acquire().await;
        match stream_recv(Pin::new(&mut udp_recv)).await {
            None => {
                let _ = ep_cmd_send.send((EpCmd::InPak(None), g));
                break;
            }
            Some(r) => {
                if let Err(_) = ep_cmd_send.send((EpCmd::InPak(Some(r)), g)) {
                    // channel closed
                    break;
                }
            }
        }
    }
}

async fn ep_task<R, U>(
    mut config: quiche::Config,
    _udp_send: Arc<U>,
    mut ep_cmd_recv: BoxRecv<
        'static,
        (EpCmd<R>, <<R as Rt>::Semaphore as Semaphore>::GuardTy),
    >,
) where
    R: Rt,
    U: Udp,
{
    let mut con_map: HashMap<quiche::ConnectionId<'static>, ConCmdSend<R>> =
        HashMap::new();

    while let Some((cmd, _g)) = ep_cmd_recv.recv().await {
        match cmd {
            EpCmd::InPak(_maybe_pak) => {}
            EpCmd::NewCon(addr, rsp) => {
                let scid = cid();
                let _con = quiche::connect(None, &scid, addr, &mut config);
                let (con, con_recv, con_cmd_send) = quiche_con(addr);
                con_map.insert(scid, con_cmd_send);
                rsp(Ok((con, con_recv)));
            }
        }
    }
}

fn cid() -> quiche::ConnectionId<'static> {
    static CID: atomic::AtomicU64 = atomic::AtomicU64::new(0);
    let id = CID.fetch_add(1, atomic::Ordering::Relaxed);
    let id = id.to_le_bytes();
    let mut id = id.to_vec();
    id.extend_from_slice(&[0xdb; 8]);
    quiche::ConnectionId::from_vec(id.to_vec())
}
