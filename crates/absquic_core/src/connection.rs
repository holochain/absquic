//! absquic_core connection types

use crate::stream::*;
use crate::util::*;
use crate::AqResult;
use std::net::SocketAddr;

/// types only relevant when implementing a quic state machine backend
pub mod backend {
    use super::*;

    /// send a control command to the connection backend implementation
    pub enum ConnectionCmd {
        /// get the remote address of this connection
        GetRemoteAddress(OneShotSender<AqResult<SocketAddr>>),

        /// open a new outgoing uni-directional stream
        OpenUniStream(OneShotSender<AqResult<WriteStream>>),
    }

    /// as a backend library, construct an absquic connection instance
    pub fn construct_connection(
        command_sender: InChanSender<ConnectionCmd>,
        event_receiver: OutChanReceiver<ConnectionEvt>,
    ) -> (Connection, ConnectionRecv) {
        (Connection(command_sender), event_receiver)
    }
}

use backend::*;

/// events related to a quic connection
pub enum ConnectionEvt {
    /// connection error, the connection will no longer function
    Error(one_err::OneErr),

    /// handshake data is read
    HandshakeDataReady,

    /// connection established
    Connected,

    /// incoming uni-directional stream
    InUniStream(ReadStream),

    /// incoming bi-directional stream
    InBiStream(WriteStream, ReadStream),

    /// incoming un-ordered datagram
    InDatagram(bytes::Bytes),
}

/// Receive events related to a specific quic connection instance
pub type ConnectionRecv = OutChanReceiver<ConnectionEvt>;

impl OutChanReceiver<ConnectionEvt> {
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
#[derive(Clone)]
pub struct Connection(InChanSender<ConnectionCmd>);

impl Connection {
    /// the current address associated with the remote side of this connection
    pub async fn remote_address(&self) -> AqResult<SocketAddr> {
        let (s, r) = one_shot_channel();
        self.0.send(ConnectionCmd::GetRemoteAddress(s))?;
        r.recv()
            .await
            .ok_or_else(|| one_err::OneErr::new("ConnectionClosed"))?
    }

    /// open a new outgoing uni-directional stream
    pub async fn open_uni_stream(&self) -> AqResult<WriteStream> {
        let (s, r) = one_shot_channel();
        self.0.send(ConnectionCmd::OpenUniStream(s))?;
        r.recv()
            .await
            .ok_or_else(|| one_err::OneErr::new("ConnectionClosed"))?
    }
}
