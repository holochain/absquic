//! Absquic_core udp backend types

use crate::*;
use std::future::Future;
use std::net::IpAddr;
use std::net::SocketAddr;

/// A udp packet, either incoming or outgoing
pub struct UdpPak {
    /// Destination address for outgoing packets,
    /// or the source address for incoming packets
    pub addr: SocketAddr,

    /// Data to send or received
    pub data: Vec<u8>,

    /// Optional source ip for outgoing packets,
    /// or destination ip for incoming packets
    pub ip: Option<IpAddr>,

    /// Optional generic send offload segment info
    pub gso: Option<usize>,

    /// Optional explicit congestion notification bits
    pub ecn: Option<u8>,
}

/// A udp backend handle
pub trait Udp: 'static + Send + Sync {
    /// CloseImmed future return type
    type CloseImmedFut: Future<Output = ()> + 'static + Send;

    /// Addr future return type
    type AddrFut: Future<Output = Result<SocketAddr>> + 'static + Send;

    /// Send future return type
    type SendFut: Future<Output = Result<()>> + 'static + Send;

    /// Immediately shutdown the socket - data in flight may be lost
    fn close_immediate(&self) -> Self::CloseImmedFut;

    /// Get the local address the udp backend socket is currently bound to
    fn addr(&self) -> Self::AddrFut;

    /// Send an outgoing udp packet
    fn send(&self, pak: UdpPak) -> Self::SendFut;
}

/// A Factory for constructing a pre-configured udp backend socket binding
pub trait UdpFactory: 'static + Send {
    /// The udp backend handle type to return on bind
    type UdpTy: Udp;

    /// The udp backend packet receiver stream to return on bind
    type UdpRecvTy: futures_core::Stream<Item = Result<UdpPak>>
        + 'static
        + Send
        + Unpin;

    /// Bind future return type
    type BindFut: Future<Output = Result<(Self::UdpTy, Self::UdpRecvTy)>>
        + 'static
        + Send;

    /// Bind a new udp backend socket
    fn bind(self) -> Self::BindFut;
}
