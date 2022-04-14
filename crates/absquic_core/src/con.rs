//! Absquic_core connection types

use crate::*;
use std::net::SocketAddr;
use std::sync::Arc;

/// Opaque reference type
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Opaque(pub u64);

impl Default for Opaque {
    fn default() -> Self {
        Opaque(u64::MAX)
    }
}

/// An outgoing http3 request
#[derive(Default)]
pub struct Http3MsgBuilder {
    headers: Vec<(Box<[u8]>, Box<[u8]>)>,
    body: Vec<bytes::Bytes>,
    reference: Opaque,
}

impl Http3MsgBuilder {
    /// Finalize this Http3Msg
    pub fn build(self) -> Http3Msg {
        Http3Msg {
            headers: self.headers.into_boxed_slice(),
            body: self.body.into_boxed_slice(),
            reference: self.reference,
        }
    }

    /// Add a header to the outbound http3 item
    pub fn add_header<N, V>(&mut self, name: N, value: V)
    where
        N: Into<Box<[u8]>>,
        V: Into<Box<[u8]>>,
    {
        self.headers.push((name.into(), value.into()));
    }

    /// Append body data to the outbound http3 item
    pub fn append_body<B>(&mut self, body: B)
    where
        B: Into<bytes::Bytes>,
    {
        self.body.push(body.into());
    }

    /// If this http3 item is a response, set the opaque reference
    pub fn set_response(&mut self, opaque: Opaque) {
        self.reference = opaque;
    }
}

/// An incoming http3 request or response
pub struct Http3Msg {
    /// Headers associated with this message
    pub headers: Box<[(Box<[u8]>, Box<[u8]>)]>,

    /// Body (if any) associated with this message
    pub body: Box<[bytes::Bytes]>,

    /// Opaque reference allowing request / response matching
    pub reference: Opaque,
}

/// Absquic connection events
pub enum ConEvt {
    /// The connection has been established
    Connected,

    /// An incoming http3 request or response
    Http3(Http3Msg),
}

/// Dynamic dispatch absquic connection events receiver type
pub type DynConRecv = BoxRecv<'static, ConEvt>;

/// Absquic connection abstraction
pub trait Con: 'static + Send + Sync {
    /// Addr future return type
    type AddrFut: Future<Output = Result<SocketAddr>> + 'static + Send;

    /// Http3Req future return type
    type Http3Fut: Future<Output = Result<()>> + 'static + Send;

    /// Convert this connection into a dynamic dispatch wrapper
    fn into_dyn(self) -> DynCon;

    /// Get the socket addr of the remote end of this connection
    fn addr(&self) -> Self::AddrFut;

    /// Make an outgoing http3 request or response
    fn http3(&self, item: Http3Msg) -> Self::Http3Fut;
}

/// Dynamic dispatch absquic connection wrapper trait
pub trait AsDynCon: 'static + Send + Sync {
    /// Get the socket addr of the remote end of this connection
    fn addr(&self) -> BoxFut<'static, Result<SocketAddr>>;

    /// Make an outgoing http3 request or response
    fn http3(&self, item: Http3Msg) -> BoxFut<'static, Result<()>>;
}

impl<C: Con> AsDynCon for C {
    #[inline(always)]
    fn addr(&self) -> BoxFut<'static, Result<SocketAddr>> {
        BoxFut::new(Con::addr(self))
    }

    #[inline(always)]
    fn http3(&self, item: Http3Msg) -> BoxFut<'static, Result<()>> {
        BoxFut::new(Con::http3(self, item))
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

    /// Make an outgoing http3 request or response
    #[inline(always)]
    pub async fn http3(&self, item: Http3Msg) -> Result<()> {
        self.0.http3(item).await
    }
}

impl Con for DynCon {
    type AddrFut = BoxFut<'static, Result<SocketAddr>>;
    type Http3Fut = BoxFut<'static, Result<()>>;

    #[inline(always)]
    fn into_dyn(self) -> DynCon {
        self
    }

    #[inline(always)]
    fn addr(&self) -> Self::AddrFut {
        self.0.addr()
    }

    #[inline(always)]
    fn http3(&self, item: Http3Msg) -> Self::Http3Fut {
        self.0.http3(item)
    }
}
