//! Types and traits for implementing absquic backends

use crate::endpoint::backend::*;
use crate::endpoint::*;
use crate::runtime::*;
use crate::util::*;
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

/// Incoming commands for the udp backend
pub enum UdpBackendCmd {
    /// Get the local address currently associated with the udp backend
    GetLocalAddress(OneShotSender<SocketAddr>),
}

/// Events emitted by the udp backend
pub enum UdpBackendEvt {
    /// The udp backend has received an incoming udp packet
    InUdpPacket(InUdpPacket),
}

/// A factory type for binding udp backends
pub trait UdpBackendFactory: 'static + Send + Sync {
    /// Bind a new udp backend socket
    fn bind<R>(
        &self,
        runtime: R,
    ) -> AqFut<
        'static,
        AqResult<(
            Sender<UdpBackendCmd>,
            Sender<OutUdpPacket>,
            Receiver<UdpBackendEvt>,
        )>,
    >
    where
        R: AsyncRuntime;
}

/// Trait defining an absquic backend driver factory
pub trait QuicBackendFactory: 'static + Send + Sync {
    /// Bind a new endpoint of this type
    fn bind<R, U>(
        &self,
        runtime: R,
        udp_backend: U,
    ) -> AqFut<'static, AqResult<(Sender<EndpointCmd>, Receiver<EndpointEvt>)>>
    where
        R: AsyncRuntime,
        U: UdpBackendFactory;
}
