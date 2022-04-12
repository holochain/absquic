//! Absquic_core connection types

use crate::*;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;

/// Quic connection events
pub enum ConEvt {
    /// The connection has been established
    Connected,
}

/// Quic connection events receiver type
pub type DynConRecv =
    Pin<Box<dyn futures_core::Stream<Item = ConEvt> + 'static + Send>>;

/// Quic connection
pub trait Con: 'static + Send + Sync {
    /// Addr future return type
    type AddrFut: Future<Output = AqResult<SocketAddr>> + 'static + Send;

    /// Convert this connection into a dynamic dispatch wrapper
    fn into_dyn(self) -> DynCon;

    /// Get the socket addr of the remote end of this connection
    fn addr(&self) -> Self::AddrFut;
}

/// Dynamic dispatch Quic connection wrapper trait
pub trait AsDynCon: 'static + Send + Sync {
    /// Get the socket addr of the remote end of this connection
    fn addr(&self) -> AqBoxFut<'static, AqResult<SocketAddr>>;
}

impl<C: Con> AsDynCon for C {
    #[inline(always)]
    fn addr(&self) -> AqBoxFut<'static, AqResult<SocketAddr>> {
        Box::pin(Con::addr(self))
    }
}

/// Dynamic dispatch Quic connection newtype
#[derive(Clone)]
pub struct DynCon(pub Arc<dyn AsDynCon + 'static + Send + Sync>);

impl DynCon {
    /// Get the socket addr of the remote end of this connection
    #[inline(always)]
    pub async fn addr(&self) -> AqResult<SocketAddr> {
        self.0.addr().await
    }
}
