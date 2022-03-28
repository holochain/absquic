//! Types and traits for implementing absquic backends

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
    /// Destination address
    pub dst_addr: SocketAddr,

    /// Source ip
    pub src_ip: Option<IpAddr>,

    /// If this packet contains multiple datagrams
    pub segment_size: Option<usize>,

    /// Explicit congestion notification
    pub ecn: Option<u8>,

    /// Would be nice if this were bytes::BytesMut,
    /// but this needs to match the quinn_proto api
    pub data: Vec<u8>,
}

/// Incoming Raw Udp Packet Struct
#[derive(Debug)]
pub struct InUdpPacket {
    /// Source address
    pub src_addr: SocketAddr,

    /// Destination ip
    pub dst_ip: Option<IpAddr>,

    /// Explicit congestion notification
    pub ecn: Option<u8>,

    /// Packet data content
    pub data: bytes::BytesMut,
}

/// Trait defining an absquic backend udp implementation
pub trait UdpBackendSender: 'static + Send {
    /// Get the local address of this backend udp socket
    fn local_addr(&mut self) -> OneShotReceiver<SocketAddr>;

    /// Get the recommended batch size for this backend udp socket
    fn batch_size(&self) -> usize;

    /// Get the max Generic Send Offload (GSO) segments
    /// usable by this backend, this value can change on gso errors
    fn max_gso_segments(&self) -> usize;

    /// Send data through this backend udp socket
    fn poll_send(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<AqResult<SenderCb<OutUdpPacket>>>;
}

impl dyn UdpBackendSender + 'static + Send {
    /// Send data through this backend udp socket
    pub async fn send(&mut self, data: OutUdpPacket) -> AqResult<()> {
        struct X<'lt>(&'lt mut (dyn UdpBackendSender + 'static + Send));

        impl<'lt> Future for X<'lt> {
            type Output = AqResult<SenderCb<OutUdpPacket>>;

            fn poll(
                mut self: std::pin::Pin<&mut Self>,
                cx: &mut Context<'_>,
            ) -> Poll<Self::Output> {
                self.0.poll_send(cx)
            }
        }

        let cb = X(self).await?;
        cb(data);
        Ok(())
    }
}

/// Trait object udp backend sender
pub type DynUdpBackendSender = Box<dyn UdpBackendSender + 'static + Send>;

/// Trait defining an absquic backend udp implementation
pub trait UdpBackendReceiver: 'static + Send {
    /// Recv data from this backend udp socket
    fn poll_recv(&mut self, cx: &mut Context<'_>) -> Poll<Option<InUdpPacket>>;
}

/// Trait object udp backend receiver
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
    /// Bind a new udp backend socket
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
    /// Construct a new absquic backend driver future from a generic future
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
    /// Construct a new endpoint triplet of this type
    fn construct_endpoint(
        &self,
        udp_backend: Arc<dyn UdpBackendFactory>,
        timeouts_scheduler: Box<dyn TimeoutsScheduler>,
    ) -> AqBoxFut<
        'static,
        AqResult<(Sender<EndpointCmd>, Receiver<EndpointEvt>, BackendDriver)>,
    >;
}
