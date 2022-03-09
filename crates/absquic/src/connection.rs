//! tx3_quic Connection

use crate::OutChan;
use crate::Tx3Result;
//use crate::types::*;
use crate::driver::*;
use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::task::Context;
use std::task::Poll;

/// Events associated with a specific connection
#[derive(Debug)]
pub enum ConnectionEvt {
    /// handshake data available to read
    HandshakeDataReady,

    /// connection established
    Connected,

    /// connection closed
    ConnectionLost(one_err::OneErr),
}

/// Stream of incoming connection events
pub struct ConnectionEvtSrc(pub(crate) OutChan<ConnectionEvt>);

impl ConnectionEvtSrc {
    /// get the next incoming connection event
    #[inline(always)]
    pub fn poll_recv(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Option<ConnectionEvt>> {
        self.0.poll_recv(cx)
    }

    /// get the next incoming connection event
    pub async fn recv(&mut self) -> Option<ConnectionEvt> {
        struct X<'lt>(&'lt mut ConnectionEvtSrc);

        impl Future for X<'_> {
            type Output = Option<ConnectionEvt>;

            fn poll(
                mut self: Pin<&mut Self>,
                cx: &mut Context<'_>,
            ) -> Poll<Self::Output> {
                self.0.poll_recv(cx)
            }
        }

        X(self).await
    }

    /// wait until this connection is connected
    pub async fn connected(mut self) -> Tx3Result<Self> {
        while let Some(evt) = self.recv().await {
            match evt {
                ConnectionEvt::HandshakeDataReady => (),
                ConnectionEvt::Connected => return Ok(self),
                oth => return Err(format!("awaiting connected, got: {:?}", oth).into()),
            }
        }
        Err("awaiting connected, got end of stream".into())
    }
}

/// Quic Connection
#[derive(Clone)]
pub struct Connection(
    pub(crate) DriverCore,
    pub(crate) quinn_proto::ConnectionHandle,
);

impl Connection {
    /// Get the current remote address this connection is bound to
    pub fn remote_address(&self) -> Tx3Result<SocketAddr> {
        self.0.con_remote_addr(self.1)
    }
}
