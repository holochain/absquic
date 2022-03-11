//! absquic_core endpoint types

use crate::connection::*;
use crate::util::*;
use crate::AqResult;
use std::net::SocketAddr;

/// types only relevant when implementing a quic state machine backend
pub mod backend {
    use super::*;

    /// send a control command to the endpoint backend implementation
    pub enum EndpointCmd {
        /// get the local addr this endpoint is bound to
        GetLocalAddress(OneShotSender<AqResult<SocketAddr>>),

        /// attempt to establish a new outgoing connection
        Connect {
            /// the address to connect to
            addr: SocketAddr,

            /// the server name to connect to
            server_name: String,

            /// cb for the result of the connection attempt
            cb: OneShotSender<AqResult<(Connection, ConnectionRecv)>>,
        },
    }

    /// as a backend library, construct an absquic endpoint instance
    pub fn construct_endpoint(
        command_sender: InChanSender<EndpointCmd>,
        event_receiver: OutChanReceiver<EndpointEvt>,
    ) -> (Endpoint, EndpointRecv) {
        (Endpoint(command_sender), event_receiver)
    }
}

use backend::*;

/// events related to a quic endpoint
pub enum EndpointEvt {
    /// endpoint error, the endpoint will no longer function
    Error(one_err::OneErr),

    /// incoming connection
    InConnection(Connection, ConnectionRecv),
}

/// Receive events related to a specific quic endpoint instance
pub type EndpointRecv = OutChanReceiver<EndpointEvt>;

/// A handle to a quic endpoint
#[derive(Clone)]
pub struct Endpoint(InChanSender<EndpointCmd>);

impl Endpoint {
    /// the current address this endpoint is bound to
    pub async fn local_address(&self) -> AqResult<SocketAddr> {
        let (s, r) = one_shot_channel();
        self.0.send(EndpointCmd::GetLocalAddress(s))?;
        r.recv()
            .await
            .ok_or_else(|| one_err::OneErr::new("EndpointClosed"))?
    }

    /// attempt to establish a new outgoing connection
    pub async fn connect(
        &self,
        addr: SocketAddr,
        server_name: String,
    ) -> AqResult<(Connection, ConnectionRecv)> {
        let (s, r) = one_shot_channel();
        self.0.send(EndpointCmd::Connect {
            addr,
            server_name,
            cb: s,
        })?;
        r.recv()
            .await
            .ok_or_else(|| one_err::OneErr::new("EndpointClosed"))?
    }
}