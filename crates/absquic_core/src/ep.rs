//! Absquic_core endpoint types

use crate::con::*;
use crate::*;
use std::net::SocketAddr;
use std::sync::Arc;

/// Absquic endpoint events
pub enum EpEvt<ConTy, ConRecvTy>
where
    ConTy: Con,
    ConRecvTy: futures_core::Stream<Item = ConEvt> + 'static + Send + Unpin,
{
    /// Received a new incoming connection
    InCon(ConTy, ConRecvTy),
}

/// Dynamic dispatch absquic endpoint events receiver type
pub type DynEpRecv = BoxRecv<'static, EpEvt<DynCon, DynConRecv>>;

/// Absquic endpoint abstraction
pub trait Ep: 'static + Send + Sync {
    /// Connection type
    type ConTy: Con;

    /// Connection event receiver type
    type ConRecvTy: futures_core::Stream<Item = ConEvt> + 'static + Send + Unpin;

    /// Addr future return type
    type AddrFut: Future<Output = Result<SocketAddr>> + 'static + Send;

    /// Connect future return type
    type ConFut: Future<Output = Result<(Self::ConTy, Self::ConRecvTy)>>
        + 'static
        + Send;

    /// Convert this endpoint into a dynamic dispatch wrapper
    fn into_dyn(self) -> DynEp;

    /// Get the socket addr currently bound by this endpoint
    fn addr(&self) -> Self::AddrFut;

    /// Establish a new outgoing connection
    fn connect(&self, addr: SocketAddr) -> Self::ConFut;
}

/// Dynamic dispatch absquic endpoint wrapper trait
pub trait AsDynEp: 'static + Send + Sync {
    /// Get the socket addr currently bound by this endpoint
    fn addr(&self) -> BoxFut<'static, Result<SocketAddr>>;

    /// Establish a new outgoing connection
    fn connect(
        &self,
        addr: SocketAddr,
    ) -> BoxFut<'static, Result<(DynCon, DynConRecv)>>;
}

impl<E: Ep> AsDynEp for E {
    #[inline(always)]
    fn addr(&self) -> BoxFut<'static, Result<SocketAddr>> {
        BoxFut::new(Ep::addr(self))
    }

    #[inline(always)]
    fn connect(
        &self,
        addr: SocketAddr,
    ) -> BoxFut<'static, Result<(DynCon, DynConRecv)>> {
        let fut = Ep::connect(self, addr);
        BoxFut::new(async move {
            let (con, recv) = fut.await?;
            let con = con.into_dyn();
            let recv: DynConRecv = BoxRecv::new(recv);
            Ok((con, recv))
        })
    }
}

/// Dynamic dispatch absquic endpoint newtype
#[derive(Clone)]
pub struct DynEp(pub Arc<dyn AsDynEp + 'static + Send + Sync>);

impl DynEp {
    /// Get the socket addr currently bound by this endpoint
    #[inline(always)]
    pub async fn addr(&self) -> Result<SocketAddr> {
        self.0.addr().await
    }

    /// Establish a new outgoing connection
    #[inline(always)]
    pub async fn connect(
        &self,
        addr: SocketAddr,
    ) -> Result<(DynCon, DynConRecv)> {
        self.0.connect(addr).await
    }
}

impl Ep for DynEp {
    type ConTy = DynCon;
    type ConRecvTy = DynConRecv;
    type AddrFut = BoxFut<'static, Result<SocketAddr>>;
    type ConFut = BoxFut<'static, Result<(Self::ConTy, Self::ConRecvTy)>>;

    #[inline(always)]
    fn into_dyn(self) -> DynEp {
        self
    }

    #[inline(always)]
    fn addr(&self) -> Self::AddrFut {
        self.0.addr()
    }

    #[inline(always)]
    fn connect(&self, addr: SocketAddr) -> Self::ConFut {
        self.0.connect(addr)
    }
}

/// A factory for constructing a pre-configured quic endpoint
pub trait EpFactory: 'static + Send {
    /// The connection backend handle type
    type ConTy: Con;

    /// The connection backend event receiver stream
    type ConRecvTy: futures_core::Stream<Item = ConEvt> + 'static + Send + Unpin;

    /// The endpoint backend handle type to return on bind
    type EpTy: Ep;

    /// The endpoint backend event receiver stream to return on bind
    type EpRecvTy: futures_core::Stream<Item = EpEvt<Self::ConTy, Self::ConRecvTy>>
        + 'static
        + Send
        + Unpin;

    /// Bind future return type
    type BindFut: Future<Output = Result<(Self::EpTy, Self::EpRecvTy)>>
        + 'static
        + Send;

    /// Bind a new quic backend endpoint
    fn bind(self) -> Self::BindFut;
}

/// Dynamic dispatch factory for constructing a pre-configured quic endpoint
pub struct DynEpFactory(
    Box<
        dyn FnOnce() -> BoxFut<'static, Result<(DynEp, DynEpRecv)>>
            + 'static
            + Send,
    >,
);

impl DynEpFactory {
    /// Construct a dynamic ep factory from a generic one
    pub fn new<F: EpFactory>(f: F) -> Self {
        Self(Box::new(move || {
            BoxFut::new(async move {
                let (ep, recv) = f.bind().await?;
                let ep = ep.into_dyn();
                let recv = BoxRecv::new(ConvEpRecv(recv));
                Ok((ep, recv))
            })
        }))
    }
}

struct ConvEpRecv<
    C: Con,
    CRecv: futures_core::Stream<Item = ConEvt> + 'static + Send + Unpin,
    Recv: futures_core::Stream<Item = EpEvt<C, CRecv>> + 'static + Send + Unpin,
>(Recv);

impl<
        C: Con,
        CRecv: futures_core::Stream<Item = ConEvt> + 'static + Send + Unpin,
        Recv: futures_core::Stream<Item = EpEvt<C, CRecv>> + 'static + Send + Unpin,
    > futures_core::Stream for ConvEpRecv<C, CRecv, Recv>
{
    type Item = EpEvt<DynCon, DynConRecv>;

    fn poll_next(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Self::Item>> {
        match Pin::new(&mut self.0).poll_next(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Ready(Some(evt)) => match evt {
                EpEvt::InCon(con, recv) => {
                    let con = con.into_dyn();
                    let recv = BoxRecv::new(recv);
                    Poll::Ready(Some(EpEvt::InCon(con, recv)))
                }
            },
        }
    }
}

impl EpFactory for DynEpFactory {
    type ConTy = DynCon;
    type ConRecvTy = DynConRecv;
    type EpTy = DynEp;
    type EpRecvTy = DynEpRecv;
    type BindFut = BoxFut<'static, Result<(DynEp, DynEpRecv)>>;
    fn bind(self) -> Self::BindFut {
        (self.0)()
    }
}
