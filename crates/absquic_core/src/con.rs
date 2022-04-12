//! Absquic_core connection types

use crate::*;
use std::net::SocketAddr;
use std::sync::Arc;

/// Absquic connection events
pub enum ConEvt {
    /// The connection has been established
    Connected,
}

/// Dynamic dispatch absquic connection events receiver type
pub type DynConRecv = BoxRecv<'static, ConEvt>;

/// Absquic connection abstraction
pub trait Con: 'static + Send + Sync {
    /// Addr future return type
    type AddrFut: Future<Output = Result<SocketAddr>> + 'static + Send;

    /// Convert this connection into a dynamic dispatch wrapper
    fn into_dyn(self) -> DynCon;

    /// Get the socket addr of the remote end of this connection
    fn addr(&self) -> Self::AddrFut;
}

/// Dynamic dispatch absquic connection wrapper trait
pub trait AsDynCon: 'static + Send + Sync {
    /// Get the socket addr of the remote end of this connection
    fn addr(&self) -> BoxFut<'static, Result<SocketAddr>>;
}

impl<C: Con> AsDynCon for C {
    #[inline(always)]
    fn addr(&self) -> BoxFut<'static, Result<SocketAddr>> {
        BoxFut::new(Con::addr(self))
    }
}

/// Dynamic dispatch absquic connection newtype
#[derive(Clone)]
pub struct DynCon(pub Arc<dyn AsDynCon + 'static + Send + Sync>);

impl DynCon {
    /// Get the socket addr of the remote end of this connection
    #[inline(always)]
    pub async fn addr(&self) -> Result<SocketAddr> {
        self.0.addr().await
    }
}

impl Con for DynCon {
    type AddrFut = BoxFut<'static, Result<SocketAddr>>;

    #[inline(always)]
    fn into_dyn(self) -> DynCon {
        self
    }

    #[inline(always)]
    fn addr(&self) -> Self::AddrFut {
        self.0.addr()
    }
}
