//! types and traits for implementing absquic backends

use crate::endpoint::backend::*;
use crate::endpoint::*;
use crate::util::*;
use crate::*;
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

    /// source ip
    pub src_ip: Option<IpAddr>,

    /// if this packet contains multiple datagrams
    pub segment_size: Option<usize>,

    /// explicit congestion notification
    pub ecn: Option<u8>,

    /// would be nice if this were bytes::BytesMut,
    /// but this needs to match the quinn_proto api
    pub data: Vec<u8>,
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
pub trait UdpBackendSender: 'static + Send + Sync {
    /// Get the local address of this backend udp socket
    fn local_addr(&self) -> AqBoxFut<'static, AqResult<SocketAddr>>;

    /// Get the recommended batch size for this backend udp socket
    fn batch_size(&self) -> usize;

    /// Get the max Generic Send Offload (GSO) segments
    /// usable by this backend, this value can change on gso errors
    fn max_gso_segments(&self) -> usize;

    /// Send data through this backend udp socket
    /// (if successfull, the data will be taken from the option)
    fn send(&self, data: OutUdpPacket) -> AqBoxFut<'static, AqResult<()>>;
}

/// trait object udp backend sender
pub type DynUdpBackendSender =
    Arc<dyn UdpBackendSender + 'static + Send + Sync>;

/// Trait defining an absquic backend udp implementation
pub trait UdpBackendReceiver: 'static + Send {
    /// Recv data from this backend udp socket
    fn poll_recv(&mut self, cx: &mut Context<'_>) -> Poll<Option<InUdpPacket>>;
}

/// trait object udp backend receiver
pub type DynUdpBackendReceiver = Box<dyn UdpBackendReceiver + 'static + Send>;

impl dyn UdpBackendReceiver + 'static + Send {
    /// Recv data from this backend udp socket
    pub async fn recv(&mut self) -> Option<InUdpPacket> {
        struct X<'lt>(&'lt mut (dyn UdpBackendReceiver + 'static + Send));

        impl<'lt> Future for X<'lt> {
            type Output = Option<InUdpPacket>;

            fn poll(
                mut self: std::pin::Pin<&mut Self>,
                cx: &mut Context<'_>,
            ) -> Poll<Self::Output> {
                self.0.poll_recv(cx)
            }
        }

        X(self).await
    }
}

/// Trait defining a factory for udp backends
pub trait UdpBackendFactory: 'static + Send + Sync {
    /// bind a new udp backend socket
    fn bind(
        &self,
    ) -> AqBoxFut<
        'static,
        AqResult<(DynUdpBackendSender, DynUdpBackendReceiver, BackendDriver)>,
    >;
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
