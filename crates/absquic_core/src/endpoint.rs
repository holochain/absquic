//! Absquic_core endpoint types

use crate::backend::*;
use crate::connection::*;
use crate::runtime::*;
use crate::*;
use std::net::SocketAddr;
use std::sync::Arc;

/// Types only relevant when implementing a quic state machine backend
pub mod backend {
    use super::*;

    /// Send a control command to the endpoint backend implementation
    pub enum EndpointCmd {
        /// Get the local addr this endpoint is bound to
        GetLocalAddress(OnceSender<SocketAddr>),

        /// Attempt to establish a new outgoing connection
        Connect {
            /// Resp sender for the result of the connection attempt
            sender: OnceSender<(Connection, MultiReceiver<ConnectionEvt>)>,

            /// The address to connect to
            addr: SocketAddr,

            /// The server name to connect to
            server_name: String,
        },
    }
}

use backend::*;

/// Events related to a quic endpoint
pub enum EndpointEvt {
    /// Endpoint error, the endpoint will no longer function
    Error(one_err::OneErr),

    /// Incoming connection
    InConnection(Connection, MultiReceiver<ConnectionEvt>),
}

type OnceChan<T> = Arc<
    dyn Fn() -> (OnceSender<T>, AqFut<'static, Option<T>>) + 'static + Send,
>;

/// A handle to a quic endpoint
#[derive(Clone)]
pub struct Endpoint {
    cmd_send: MultiSender<EndpointCmd>,

    // these handles let us avoid having the runtime generic on this type
    one_shot_socket_addr: OnceChan<SocketAddr>,
    one_shot_connect: OnceChan<(Connection, MultiReceiver<ConnectionEvt>)>,
}

impl Endpoint {
    /// Construct a new absquic endpoint
    pub async fn new<Runtime, Udp, Quic>(
        udp_backend: Udp,
        quic_backend: Quic,
    ) -> AqResult<(Self, MultiReceiver<EndpointEvt>)>
    where
        Runtime: AsyncRuntime,
        Udp: UdpBackendFactory,
        Quic: QuicBackendFactory,
    {
        let (cmd_send, evt_recv) =
            quic_backend.bind::<Runtime, _>(udp_backend).await?;

        let one_shot_socket_addr = Arc::new(|| Runtime::one_shot());
        let one_shot_connect = Arc::new(|| Runtime::one_shot());

        Ok((
            Self {
                cmd_send,
                one_shot_socket_addr,
                one_shot_connect,
            },
            evt_recv,
        ))
    }

    /// The current address this endpoint is bound to
    pub async fn local_address(&mut self) -> ChanResult<SocketAddr> {
        let (s, r) = (self.one_shot_socket_addr)();
        self.cmd_send
            .acquire()
            .await?
            .send(EndpointCmd::GetLocalAddress(s));
        r.await.ok_or(ChannelClosed)
    }

    /// Attempt to establish a new outgoing connection
    pub async fn connect(
        &mut self,
        addr: SocketAddr,
        server_name: &str,
    ) -> ChanResult<(Connection, MultiReceiver<ConnectionEvt>)> {
        let (s, r) = (self.one_shot_connect)();
        self.cmd_send.acquire().await?.send(EndpointCmd::Connect {
            sender: s,
            addr,
            server_name: server_name.to_string(),
        });
        r.await.ok_or(ChannelClosed)
    }
}
