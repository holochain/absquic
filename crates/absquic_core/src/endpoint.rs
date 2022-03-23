//! absquic_core endpoint types

use crate::backend::*;
use crate::connection::*;
use crate::util::*;
use crate::AqResult;
use std::net::SocketAddr;
use std::sync::Arc;

/// types only relevant when implementing a quic state machine backend
pub mod backend {
    use super::*;

    /// send a control command to the endpoint backend implementation
    pub enum EndpointCmd {
        /// get the local addr this endpoint is bound to
        GetLocalAddress(OneShotSender<SocketAddr>),

        /// attempt to establish a new outgoing connection
        Connect {
            /// resp sender for the result of the connection attempt
            sender: OneShotSender<(Connection, ConnectionRecv)>,

            /// the address to connect to
            addr: SocketAddr,

            /// the server name to connect to
            server_name: String,
        },
    }
}

use backend::*;

/// helper logic to plug in periodic scheduling into arbitrary runtime
pub trait TimeoutsScheduler: 'static + Send {
    /// schedule logic to be invoked at instant
    /// if previous logic has been scheduled but not yet triggered,
    /// it is okay to drop that previous logic without triggering.
    fn schedule(
        &mut self,
        logic: Box<dyn FnOnce() + 'static + Send>,
        at: std::time::Instant,
    );
}

/// events related to a quic endpoint
pub enum EndpointEvt {
    /// endpoint error, the endpoint will no longer function
    Error(one_err::OneErr),

    /// incoming connection
    InConnection(Connection, ConnectionRecv),
}

/// Receive events related to a specific quic endpoint instance
pub type EndpointRecv = Receiver<EndpointEvt>;

/// A handle to a quic endpoint
#[derive(Clone)]
pub struct Endpoint(Sender<EndpointCmd>);

impl Endpoint {
    /// the current address this endpoint is bound to
    pub async fn local_address(&mut self) -> AqResult<SocketAddr> {
        let (s, r) = one_shot_channel();
        self.0.send(EndpointCmd::GetLocalAddress(s)).await?;
        r.await
    }

    /// attempt to establish a new outgoing connection
    pub async fn connect(
        &mut self,
        addr: SocketAddr,
        server_name: &str,
    ) -> AqResult<(Connection, ConnectionRecv)> {
        let (s, r) = one_shot_channel();
        self.0
            .send(EndpointCmd::Connect {
                sender: s,
                addr,
                server_name: server_name.to_string(),
            })
            .await?;
        r.await
    }
}

/// a factory that can construct absquic endpoints
pub struct EndpointFactory {
    udp_backend: Arc<dyn UdpBackendFactory>,
    quic_backend: Arc<dyn BackendDriverFactory>,
}

impl EndpointFactory {
    /// construct a new endpoint factory
    pub fn new<U, D>(udp_backend: U, backend_driver: D) -> Self
    where
        U: UdpBackendFactory,
        D: BackendDriverFactory,
    {
        let udp_backend: Arc<dyn UdpBackendFactory> = Arc::new(udp_backend);
        let quic_backend: Arc<dyn BackendDriverFactory> =
            Arc::new(backend_driver);
        Self {
            udp_backend,
            quic_backend,
        }
    }

    /// bind a new endpoint from this factory
    pub async fn bind<S>(
        &self,
        timeouts_scheduler: S,
    ) -> AqResult<(Endpoint, EndpointRecv, BackendDriver)>
    where
        S: TimeoutsScheduler,
    {
        let timeouts_scheduler: Box<dyn TimeoutsScheduler> =
            Box::new(timeouts_scheduler);
        let (cmd_send, evt_recv, driver) = self
            .quic_backend
            .construct_endpoint(self.udp_backend.clone(), timeouts_scheduler)
            .await?;
        Ok((Endpoint(cmd_send), evt_recv, driver))
    }
}
