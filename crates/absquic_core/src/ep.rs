//! Absquic_core endpoint types

use crate::con::*;
use crate::rt::*;
use crate::udp::*;
use crate::*;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;

/// Quic endpoint events
pub enum EpEvt<ConTy, ConRecvTy>
where
    ConTy: Con,
    ConRecvTy: futures_core::Stream<Item = ConEvt>,
{
    /// Received a new incoming connection
    InCon(ConTy, ConRecvTy),
}

/// Quic endpoint events receiver type
pub type DynEpRecv = Pin<
    Box<
        dyn futures_core::Stream<Item = EpEvt<DynCon, DynConRecv>>
            + 'static
            + Send,
    >,
>;

/// Quic endpoint
pub trait Ep: 'static + Send + Sync {
    /// Connection type
    type ConTy: Con;

    /// Connection event receiver type
    type ConRecvTy: futures_core::Stream<Item = ConEvt> + 'static + Send;

    /// Addr future return type
    type AddrFut: Future<Output = AqResult<SocketAddr>> + 'static + Send;

    /// Connect future return type
    type ConFut: Future<Output = AqResult<(Self::ConTy, Self::ConRecvTy)>>
        + 'static
        + Send;

    /// Convert this endpoint into a dynamic dispatch wrapper
    fn into_dyn(self) -> DynEp;

    /// Get the socket addr currently bound by this endpoint
    fn addr(&self) -> Self::AddrFut;

    /// Establish a new outgoing connection
    fn connect(&self, addr: SocketAddr) -> Self::ConFut;
}

/// Dynamic dispatch Quic endpoint wrapper trait
pub trait AsDynEp: 'static + Send + Sync {
    /// Get the socket addr currently bound by this endpoint
    fn addr(&self) -> AqBoxFut<'static, AqResult<SocketAddr>>;

    /// Establish a new outgoing connection
    fn connect(
        &self,
        addr: SocketAddr,
    ) -> AqBoxFut<'static, AqResult<(DynCon, DynConRecv)>>;
}

impl<E: Ep> AsDynEp for E {
    #[inline(always)]
    fn addr(&self) -> AqBoxFut<'static, AqResult<SocketAddr>> {
        Box::pin(Ep::addr(self))
    }

    #[inline(always)]
    fn connect(
        &self,
        addr: SocketAddr,
    ) -> AqBoxFut<'static, AqResult<(DynCon, DynConRecv)>> {
        let fut = Ep::connect(self, addr);
        Box::pin(async move {
            let (con, recv) = fut.await?;
            let con = con.into_dyn();
            let recv: DynConRecv = Box::pin(recv);
            Ok((con, recv))
        })
    }
}

/// Dynamic dispatch Quic endpoint newtype
#[derive(Clone)]
pub struct DynEp(pub Arc<dyn AsDynEp + 'static + Send + Sync>);

impl DynEp {
    /// Get the socket addr currently bound by this endpoint
    #[inline(always)]
    pub async fn addr(&self) -> AqResult<SocketAddr> {
        self.0.addr().await
    }

    /// Establish a new outgoing connection
    #[inline(always)]
    pub async fn connect(
        &self,
        addr: SocketAddr,
    ) -> AqResult<(DynCon, DynConRecv)> {
        self.0.connect(addr).await
    }
}

/// A Factory for constructing a pre-configured quic endpoint
pub trait EpFactory: 'static + Send {
    /// The connection backend handle type
    type ConTy: Con;

    /// The connection backend event receiver stream
    type ConRecvTy: futures_core::Stream<Item = ConEvt> + 'static + Send;

    /// The endpoint backend handle type to return on bind
    type EpTy: Ep;

    /// The endpoint backend event receiver stream to return on bind
    type EpRecvTy: futures_core::Stream<Item = EpEvt<Self::ConTy, Self::ConRecvTy>>
        + 'static
        + Send;

    /// Bind future return type
    type BindFut: Future<Output = AqResult<(Self::EpTy, Self::EpRecvTy)>>
        + 'static
        + Send;

    /// Bind a new quic backend endpoint
    fn bind<R: Rt, U: UdpFactory>(self, udp: U) -> Self::BindFut;
}
