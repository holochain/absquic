#![allow(clippy::type_complexity)]
//! Types and traits for implementing absquic backends

use crate::endpoint::backend::*;
use crate::endpoint::*;
use crate::runtime::*;
use crate::*;
use std::net::IpAddr;
use std::net::SocketAddr;

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

/// Commands requests for the udp backend socket
pub enum UdpBackendCmd {
    /// Immediately shutdown the socket - data in flight may be lost
    CloseImmediate,

    /// Get the local address the udp backend socket is currently bound to
    GetLocalAddress(OnceSender<AqResult<SocketAddr>>),
}

impl MultiSender<UdpBackendCmd> {
    /// Immediately shutdown the socket - data in flight may be lost
    pub async fn close_immediate(&self) {
        if let Ok(sender) = self.acquire().await {
            sender.send(UdpBackendCmd::CloseImmediate);
        }
    }

    /// Get the local address the udp backend socket is currently bound to
    pub async fn get_local_address<Runtime: AsyncRuntime>(
        &self,
    ) -> AqResult<SocketAddr> {
        let (s, r) = Runtime::one_shot();
        self.acquire()
            .await?
            .send(UdpBackendCmd::GetLocalAddress(s));
        r.await.ok_or(ChannelClosed)?
    }
}

/// Events emitted by the udp backend socket
pub enum UdpBackendEvt {
    /// A udp packet was received by the udp backend socket
    InUdpPacket(InUdpPacket),
}

/// Trait defining a factory for udp backends
pub trait UdpBackendFactory: 'static + Send + Sync {
    /// Bind a new udp backend socket
    fn bind<Runtime: AsyncRuntime>(
        self,
    ) -> AqFut<
        'static,
        AqResult<(
            MultiSender<UdpBackendCmd>,
            MultiSender<OutUdpPacket>,
            MultiReceiver<UdpBackendEvt>,
        )>,
    >;
}

/// Trait defining a factory for absquic backends
pub trait QuicBackendFactory: 'static + Send + Sync {
    /// Bind a new absquic backend
    fn bind<Runtime: AsyncRuntime, Udp: UdpBackendFactory>(
        self,
        udp_backend: Udp,
    ) -> AqFut<
        'static,
        AqResult<(MultiSender<EndpointCmd>, MultiReceiver<EndpointEvt>)>,
    >;
}
