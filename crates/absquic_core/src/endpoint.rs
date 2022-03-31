//! Absquic_core endpoint types

use crate::backend::*;
use crate::connection::*;
use crate::runtime::*;
use crate::util::*;
use crate::AqResult;
use std::net::SocketAddr;

/// Types only relevant when implementing a quic state machine backend
pub mod backend {
    use super::*;

    /// Send a control command to the endpoint backend implementation
    pub enum EndpointCmd {
        /// Get the local addr this endpoint is bound to
        GetLocalAddress(OneShotSender<SocketAddr>),

        /// Attempt to establish a new outgoing connection
        Connect {
            /// Resp sender for the result of the connection attempt
            sender: OneShotSender<(Connection, ConnectionRecv)>,

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
    InConnection(Connection, ConnectionRecv),
}

/// A handle to a quic endpoint
#[derive(Clone)]
pub struct Endpoint(Sender<EndpointCmd>);

impl Endpoint {
    /// Bind a new endpoint
    pub async fn bind<R, U, Q>(
        runtime: R,
        udp_backend: U,
        quic_backend: Q,
    ) -> AqResult<(Endpoint, Receiver<EndpointEvt>)>
    where
        R: AsyncRuntime,
        U: UdpBackendFactory,
        Q: QuicBackendFactory,
    {
        let (cmd_send, evt_recv) =
            quic_backend.bind(runtime, udp_backend).await?;
        Ok((Endpoint(cmd_send), evt_recv))
    }

    /// The current address this endpoint is bound to
    pub async fn local_address(&mut self) -> AqResult<SocketAddr> {
        let (s, r) = one_shot_channel();
        self.0.send().await?(EndpointCmd::GetLocalAddress(s));
        r.await
    }

    /// Attempt to establish a new outgoing connection
    pub async fn connect(
        &mut self,
        addr: SocketAddr,
        server_name: &str,
    ) -> AqResult<(Connection, ConnectionRecv)> {
        let (s, r) = one_shot_channel();
        self.0.send().await?(EndpointCmd::Connect {
            sender: s,
            addr,
            server_name: server_name.to_string(),
        });
        r.await
    }
}
