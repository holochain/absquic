//! types and traits for implementing absquic backends

use crate::endpoint::backend::*;
use crate::endpoint::*;
use crate::util::*;
use crate::*;
use std::collections::VecDeque;
use std::future::Future;
use std::net::IpAddr;
use std::net::SocketAddr;
use std::sync::Arc;
use std::task::Context;
use std::task::Poll;

/// Outgoing Raw Udp Packet Struct
///
/// Note, this type is currently a bit of a leaked abstraction,
/// but for efficiency reasons, we need it to be compatible
/// with the Transmit type in quinn-proto
#[derive(Debug)]
pub struct OutUdpPacket {
    /// destination address
    pub dst_addr: SocketAddr,

    /// explicit congestion notification
    pub ecn: Option<u8>,

    /// would be nice if this were bytes::BytesMut,
    /// but this needs to match the quinn_proto api
    pub data: Vec<u8>,

    /// if this packet contains multiple datagrams
    pub segment_size: Option<usize>,

    /// source ip
    pub src_ip: Option<IpAddr>,
}

/// Incoming Raw Udp Packet Struct
#[derive(Debug)]
pub struct InUdpPacket {
    /// source address
    pub src_addr: SocketAddr,

    /// destination ip
    pub dst_ip: Option<IpAddr>,

    /// explicit congestion notification
    pub ecn: Option<u8>,

    /// packet data content
    pub data: bytes::BytesMut,
}

/// Trait defining an absquic backend udp implementation
pub trait UdpBackend: 'static + Send {
    /// Get the local address of this backend udp socket
    fn local_addr(&self) -> AqResult<SocketAddr>;

    /// Get the recommended batch size for this backend udp socket
    fn batch_size(&self) -> usize;

    /// Get the max Generic Send Offload (GSO) segments
    /// usable by this backend, this value can change on gso errors
    fn max_gso_segments(&self) -> usize;

    /// Send data through this backend udp socket
    /// will return `Poll::Ready(Ok(()))` only if at least
    /// one unit was popped off the front of the data queue
    fn poll_send(
        &mut self,
        cx: &mut Context<'_>,
        data: &mut VecDeque<OutUdpPacket>,
    ) -> Poll<AqResult<()>>;

    /// Recv data from this backend udp socket
    fn poll_recv(
        &mut self,
        cx: &mut Context<'_>,
        data: &mut VecDeque<InUdpPacket>,
    ) -> Poll<AqResult<()>>;
}

/// Trait defining a factory for udp backends
pub trait UdpBackendFactory: 'static + Send + Sync {
    /// bind a new udp backend socket
    fn bind(&self) -> AqBoxFut<'static, AqResult<Box<dyn UdpBackend>>>;
}

/// An absquic backend driver future
#[must_use = "futures do nothing unless you `.await` or poll them"]
pub struct BackendDriver(AqBoxFut<'static, ()>);

impl BackendDriver {
    /// construct a new absquic backend driver future from a generic future
    pub fn new<F>(f: F) -> Self
    where
        F: Future<Output = ()> + 'static + Send,
    {
        Self(Box::pin(f))
    }
}

impl Future for BackendDriver {
    type Output = ();

    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Self::Output> {
        Future::poll(self.0.as_mut(), cx)
    }
}

/// Trait defining an absquic backend driver factory
pub trait BackendDriverFactory: 'static + Send + Sync {
    /// construct a new endpoint triplet of this type
    fn construct_endpoint(
        &self,
        udp_backend: Arc<dyn UdpBackendFactory>,
    ) -> AqBoxFut<
        'static,
        AqResult<(
            InChanSender<EndpointCmd>,
            OutChanReceiver<EndpointEvt>,
            BackendDriver,
        )>,
    >;
}

pub mod util;
