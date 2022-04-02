//! Absquic_core connection types

use crate::runtime::*;
use crate::stream::*;
use crate::*;
use std::net::SocketAddr;

/// Types only relevant when implementing a quic state machine backend
pub mod backend {
    use super::*;

    /// Send a control command to the connection backend implementation
    pub enum ConnectionCmd {
        /// Get the remote address of this connection
        GetRemoteAddress(OnceSender<SocketAddr>),

        /// Open a new outgoing uni-directional stream
        OpenUniStream(OnceSender<WriteStream>),

        /// Open a new outgoing bi-directional stream
        OpenBiStream(OnceSender<(WriteStream, ReadStream)>),
    }

    /// As a backend library, construct an absquic connection instance
    pub fn construct_connection<Runtime: AsyncRuntime>() -> (
        Connection,
        MultiReceiver<ConnectionCmd>,
        MultiSender<ConnectionEvt>,
        MultiReceiver<ConnectionEvt>,
    ) {
        let (cmd_send, cmd_recv) = Runtime::channel(32);
        let (evt_send, evt_recv) = Runtime::channel(32);

        (
            Connection::new::<Runtime>(cmd_send),
            cmd_recv,
            evt_send,
            evt_recv,
        )
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

impl MultiReceiver<ConnectionEvt> {
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

type OnceChan<T> = Box<
    dyn Fn() -> (OnceSender<T>, AqFut<'static, Option<T>>) + 'static + Send,
>;

/// A handle to a quic connection
pub struct Connection {
    cmd_send: MultiSender<ConnectionCmd>,

    // these handles let us avoid having the runtime generic on this type
    one_shot_socket_addr: OnceChan<SocketAddr>,
    one_shot_write_stream: OnceChan<WriteStream>,
    one_shot_bi_stream: OnceChan<(WriteStream, ReadStream)>,
}

impl Connection {
    fn new<Runtime: AsyncRuntime>(
        cmd_send: MultiSender<ConnectionCmd>,
    ) -> Self {
        let one_shot_socket_addr = Box::new(|| Runtime::one_shot());
        let one_shot_write_stream = Box::new(|| Runtime::one_shot());
        let one_shot_bi_stream = Box::new(|| Runtime::one_shot());
        Self {
            cmd_send,
            one_shot_socket_addr,
            one_shot_write_stream,
            one_shot_bi_stream,
        }
    }

    /// The current address associated with the remote side of this connection
    pub async fn remote_address(&mut self) -> ChanResult<SocketAddr> {
        let (s, r) = (self.one_shot_socket_addr)();
        self.cmd_send
            .acquire()
            .await?
            .send(ConnectionCmd::GetRemoteAddress(s));
        r.await.ok_or(ChannelClosed)
    }

    /// Open a new outgoing uni-directional stream
    pub async fn open_uni_stream(&mut self) -> ChanResult<WriteStream> {
        let (s, r) = (self.one_shot_write_stream)();
        self.cmd_send
            .acquire()
            .await?
            .send(ConnectionCmd::OpenUniStream(s));
        r.await.ok_or(ChannelClosed)
    }

    /// Open a new outgoing bi-directional stream
    pub async fn open_bi_stream(
        &mut self,
    ) -> ChanResult<(WriteStream, ReadStream)> {
        let (s, r) = (self.one_shot_bi_stream)();
        self.cmd_send
            .acquire()
            .await?
            .send(ConnectionCmd::OpenBiStream(s));
        r.await.ok_or(ChannelClosed)
    }
}
