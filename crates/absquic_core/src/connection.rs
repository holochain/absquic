//! Absquic_core connection types

use crate::stream::*;
use crate::util::*;
use crate::AqResult;
use std::net::SocketAddr;

/// Types only relevant when implementing a quic state machine backend
pub mod backend {
    use super::*;

    /// Send a control command to the connection backend implementation
    pub enum ConnectionCmd {
        /// Get the remote address of this connection
        GetRemoteAddress(OneShotSender<SocketAddr>),

        /// Open a new outgoing uni-directional stream
        OpenUniStream(OneShotSender<WriteStream>),

        /// Open a new outgoing bi-directional stream
        OpenBiStream(OneShotSender<(WriteStream, ReadStream)>),
    }

    /// As a backend library, construct an absquic connection instance
    pub fn construct_connection(
        command_sender: Sender<ConnectionCmd>,
        event_receiver: Receiver<ConnectionEvt>,
    ) -> (Connection, ConnectionRecv) {
        (Connection(command_sender), event_receiver)
    }
}

use backend::*;

/// Events related to a quic connection
pub enum ConnectionEvt {
    /// Connection error, the connection will no longer function
    Error(one_err::OneErr),

    /// Handshake data is read
    HandshakeDataReady,

    /// Connection established
    Connected,

    /// Incoming uni-directional stream
    InUniStream(ReadStream),

    /// Incoming bi-directional stream
    InBiStream(WriteStream, ReadStream),

    /// Incoming un-ordered datagram
    InDatagram(bytes::Bytes),
}

/// Receive events related to a specific quic connection instance
pub type ConnectionRecv = Receiver<ConnectionEvt>;

impl Receiver<ConnectionEvt> {
    /// Wait for this connection to be connected
    pub async fn wait_connected(mut self) -> AqResult<Self> {
        while let Some(evt) = self.recv().await {
            match evt {
                ConnectionEvt::HandshakeDataReady => (),
                ConnectionEvt::Connected => {
                    return Ok(self);
                }
                _ => return Err("UnexpectedEvent".into()),
            }
        }
        Err("UnexpectedEOF".into())
    }
}

/// A handle to a quic connection
pub struct Connection(Sender<ConnectionCmd>);

impl Connection {
    /// The current address associated with the remote side of this connection
    pub async fn remote_address(&mut self) -> AqResult<SocketAddr> {
        let (s, r) = one_shot_channel();
        self.0.send().await?(ConnectionCmd::GetRemoteAddress(s));
        r.await
    }

    /// Open a new outgoing uni-directional stream
    pub async fn open_uni_stream(&mut self) -> AqResult<WriteStream> {
        let (s, r) = one_shot_channel();
        self.0.send().await?(ConnectionCmd::OpenUniStream(s));
        r.await
    }

    /// Open a new outgoing bi-directional stream
    pub async fn open_bi_stream(
        &mut self,
    ) -> AqResult<(WriteStream, ReadStream)> {
        let (s, r) = one_shot_channel();
        self.0.send().await?(ConnectionCmd::OpenBiStream(s));
        r.await
    }
}
