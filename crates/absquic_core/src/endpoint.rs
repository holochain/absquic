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
        GetLocalAddress(OnceSender<AqResult<SocketAddr>>, DynSemaphoreGuard),

        /// Attempt to establish a new outgoing connection
        Connect {
            /// Resp sender for the result of the connection attempt
            sender: OnceSender<
                AqResult<(Connection, MultiReceiver<ConnectionEvt>)>,
            >,

            /// The address to connect to
            addr: SocketAddr,

            /// The server name to connect to
            server_name: String,

            /// The command semaphore guard
            guard: DynSemaphoreGuard,
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
    dyn Fn() -> (OnceSender<T>, AqFut<'static, Option<T>>)
        + 'static
        + Send
        + Sync,
>;

/// A handle to a quic endpoint
#[derive(Clone)]
pub struct Endpoint {
    limit: DynSemaphore,
    cmd_send: MultiSender<EndpointCmd>,

    // these handles let us avoid having the runtime generic on this type
    one_shot_socket_addr: OnceChan<AqResult<SocketAddr>>,
    one_shot_connect:
        OnceChan<AqResult<(Connection, MultiReceiver<ConnectionEvt>)>>,
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
        let limit = Runtime::semaphore(CMD_LIMIT);

        let (cmd_send, evt_recv) =
            quic_backend.bind::<Runtime, _>(udp_backend).await?;

        let one_shot_socket_addr = Arc::new(Runtime::one_shot);
        let one_shot_connect = Arc::new(Runtime::one_shot);

        Ok((
            Self {
                limit,
                cmd_send,
                one_shot_socket_addr,
                one_shot_connect,
            },
            evt_recv,
        ))
    }

    /// The current address this endpoint is bound to
    pub async fn local_address(&self) -> AqResult<SocketAddr> {
        let guard = self.limit.acquire().await;
        let (s, r) = (self.one_shot_socket_addr)();
        self.cmd_send
            .acquire()
            .await?
            .send(EndpointCmd::GetLocalAddress(s, guard));
        r.await.ok_or(ChannelClosed)?
    }

    /// Attempt to establish a new outgoing connection
    pub async fn connect(
        &self,
        addr: SocketAddr,
        server_name: &str,
    ) -> AqResult<(Connection, MultiReceiver<ConnectionEvt>)> {
        let guard = self.limit.acquire().await;
        let (s, r) = (self.one_shot_connect)();
        self.cmd_send.acquire().await?.send(EndpointCmd::Connect {
            sender: s,
            addr,
            server_name: server_name.to_string(),
            guard,
        });
        r.await.ok_or(ChannelClosed)?
    }
}
