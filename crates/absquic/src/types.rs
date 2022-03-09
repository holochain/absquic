//! items you probably don't need unless you're implementing a custom backend

use crate::Tx3Result;
pub use bytes::BytesMut;
pub use quinn_proto::EcnCodepoint;
pub use quinn_proto::Transmit;
use std::collections::VecDeque;
use std::net::IpAddr;
use std::net::SocketAddr;
use std::task::Context;
use std::task::Poll;

/// An Incoming Udp Packet
pub struct InPacket {
    /// origin of this packet
    pub src_addr: SocketAddr,

    /// destination of this packet
    pub dst_ip: Option<IpAddr>,

    /// explicit congestion notification
    pub ecn: Option<EcnCodepoint>,

    /// data received
    pub data: BytesMut,
}

/// Trait defining a tx3_quic backend udp implementation
pub trait Backend: 'static + Send {
    /// Get the local address of this backend udp socket
    fn local_addr(&self) -> Tx3Result<SocketAddr>;

    /// Get the recommended batch size for this backend udp socket
    fn batch_size(&self) -> usize;

    /// Get the max Generic Send Offload (GSO) segments
    /// usable by this backend, this value can change on gso errors
    fn max_gso_segments(&self) -> usize;

    /// Send data through this backend udp socket
    /// will return `Poll::Ready(Ok(()))` only if at least
    /// one unit was popped off the front of the data queue
    /// will only transmit a max of `batch_size` items in one invocation
    fn poll_send(
        &mut self,
        cx: &mut Context<'_>,
        data: &mut VecDeque<Transmit>,
    ) -> Poll<Tx3Result<()>>;

    /// Recv data from this backend udp socket
    /// will not fill queue beyond `limit` items
    fn poll_recv(
        &mut self,
        cx: &mut Context<'_>,
        data: &mut VecDeque<InPacket>,
        limit: usize,
    ) -> Poll<Tx3Result<usize>>;
}
