#![deny(missing_docs)]
#![deny(warnings)]
#![deny(unsafe_code)]
//! absquic backend powered by quinn-proto

use absquic_core::backend::*;
use absquic_core::connection::backend::*;
use absquic_core::connection::*;
use absquic_core::deps::{one_err, parking_lot};
use absquic_core::endpoint::backend::*;
use absquic_core::endpoint::*;
use absquic_core::stream::backend::*;
use absquic_core::stream::*;
use absquic_core::util::*;
use absquic_core::*;
use std::collections::HashMap;
use std::collections::VecDeque;
use std::future::Future;
use std::sync::Arc;
use std::task::Context;
use std::task::Poll;

// buffer size for channels - arbitrary, needs experiments
const BUF_CAP: usize = 64;

// buffer size for read / write streams - arbitrary, needs experiments
const BYTES_CAP: usize = 1024 * 16;

#[derive(Clone)]
struct WakerSlot(Arc<parking_lot::Mutex<Option<std::task::Waker>>>);

impl WakerSlot {
    fn new() -> Self {
        Self(Arc::new(parking_lot::Mutex::new(None)))
    }

    fn set(&self, waker: std::task::Waker) {
        *self.0.lock() = Some(waker);
    }

    fn wake(&self) {
        if let Some(waker) = self.0.lock().take() {
            waker.wake();
        }
    }
}

/// absquic backend powered by quinn-proto
pub struct QuinnDriverFactory {
    endpoint_config: Arc<quinn_proto::EndpointConfig>,
    server_config: Option<Arc<quinn_proto::ServerConfig>>,
    client_config: Arc<quinn_proto::ClientConfig>,
}

impl QuinnDriverFactory {
    /// construct a new absquic driver factory backed by quinn-proto
    pub fn new(
        endpoint_config: quinn_proto::EndpointConfig,
        server_config: Option<quinn_proto::ServerConfig>,
        client_config: quinn_proto::ClientConfig,
    ) -> Self {
        Self {
            endpoint_config: Arc::new(endpoint_config),
            server_config: server_config.map(Arc::new),
            client_config: Arc::new(client_config),
        }
    }
}

impl BackendDriverFactory for QuinnDriverFactory {
    fn construct_endpoint(
        &self,
        udp_backend: Arc<dyn UdpBackendFactory>,
        timeouts_scheduler: Box<dyn TimeoutsScheduler>,
    ) -> AqBoxFut<
        'static,
        AqResult<(Sender<EndpointCmd>, Receiver<EndpointEvt>, BackendDriver)>,
    > {
        let endpoint_config = self.endpoint_config.clone();
        let server_config = self.server_config.clone();
        let client_config = self.client_config.clone();
        Box::pin(async move {
            let (udp_send, udp_recv, udp_driver) = udp_backend.bind().await?;

            let endpoint =
                quinn_proto::Endpoint::new(endpoint_config, server_config);

            let (cmd_send, cmd_recv) = channel(BUF_CAP);
            let (evt_send, evt_recv) = channel(BUF_CAP);

            let driver = QuinnDriver {
                waker: WakerSlot::new(),
                udp_driver,
                udp_send,
                udp_send_buf: VecDeque::with_capacity(BUF_CAP),
                udp_recv,
                timeouts_scheduler,
                client_config,
                endpoint,
                connections: HashMap::new(),
                cmd_recv,
                evt_send,
                evt_send_buf: VecDeque::with_capacity(BUF_CAP),
                next_timeout: std::time::Instant::now(),
            };

            let driver = BackendDriver::new(driver);

            Ok((cmd_send, evt_recv, driver))
        })
    }
}

#[derive(Clone, Copy)]
enum Disposition {
    /// it's safe to return pending, i.e. we have wakers registered
    /// everywhere needed to ensure continued function of the driver
    PendOk,

    /// we need another poll loop to continue safely
    MoreWork,
}

impl Disposition {
    pub fn merge(&mut self, oth: Self) {
        use Disposition::*;
        *self = match (*self, oth) {
            (MoreWork, _) => MoreWork,
            (_, MoreWork) => MoreWork,
            (PendOk, PendOk) => PendOk,
        }
    }
}

enum StreamInfo {
    UniOut(WriteStreamBackend),
    UniIn(ReadStreamBackend),
    Bi(WriteStreamBackend, ReadStreamBackend),
}

struct ConnectionInfo {
    connection: quinn_proto::Connection,
    streams: HashMap<quinn_proto::StreamId, StreamInfo>,
    cmd_recv: Receiver<ConnectionCmd>,
    evt_send: Sender<ConnectionEvt>,
    evt_send_buf: VecDeque<ConnectionEvt>,
    uni_out_buf: Option<OneShotSender<WriteStream>>,
    bi_buf: Option<OneShotSender<(WriteStream, ReadStream)>>,
}

impl ConnectionInfo {
    fn intake_uni_out(
        &mut self,
        stream_id: quinn_proto::StreamId,
    ) -> WriteStream {
        let (wb, wf) = write_stream_pair(BYTES_CAP);
        self.streams.insert(stream_id, StreamInfo::UniOut(wb));
        wf
    }

    fn intake_uni_in(
        &mut self,
        stream_id: quinn_proto::StreamId,
    ) -> ReadStream {
        let (rb, rf) = read_stream_pair(BYTES_CAP);
        self.streams.insert(stream_id, StreamInfo::UniIn(rb));
        rf
    }

    fn intake_bi(
        &mut self,
        stream_id: quinn_proto::StreamId,
    ) -> (WriteStream, ReadStream) {
        let (wb, wf) = write_stream_pair(BYTES_CAP);
        let (rb, rf) = read_stream_pair(BYTES_CAP);
        self.streams.insert(stream_id, StreamInfo::Bi(wb, rb));
        (wf, rf)
    }
}

struct QuinnDriver {
    waker: WakerSlot,
    udp_driver: BackendDriver,
    udp_send: DynUdpBackendSender,
    udp_send_buf: VecDeque<OutUdpPacket>,
    udp_recv: DynUdpBackendReceiver,
    timeouts_scheduler: Box<dyn TimeoutsScheduler>,
    client_config: Arc<quinn_proto::ClientConfig>,
    endpoint: quinn_proto::Endpoint,
    connections: HashMap<quinn_proto::ConnectionHandle, ConnectionInfo>,
    cmd_recv: Receiver<EndpointCmd>,
    evt_send: Sender<EndpointEvt>,
    evt_send_buf: VecDeque<EndpointEvt>,
    next_timeout: std::time::Instant,
}

impl Future for QuinnDriver {
    type Output = ();

    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Self::Output> {
        match self.poll_inner(cx) {
            Err(e) => {
                tracing::error!("{:?}", e);
                Poll::Ready(())
            }
            Ok(r) => r,
        }
    }
}

mod poll_connections;

impl QuinnDriver {
    pub fn intake_connection(
        &mut self,
        hnd: quinn_proto::ConnectionHandle,
        con: quinn_proto::Connection,
    ) -> AqResult<(Connection, ConnectionRecv)> {
        let (cmd_send, cmd_recv) = channel(BUF_CAP);
        let (evt_send, evt_recv) = channel(BUF_CAP);
        self.connections.insert(
            hnd,
            ConnectionInfo {
                connection: con,
                streams: HashMap::new(),
                cmd_recv,
                evt_send,
                evt_send_buf: VecDeque::with_capacity(BUF_CAP),
                uni_out_buf: None,
                bi_buf: None,
            },
        );
        Ok(construct_connection(cmd_send, evt_recv))
    }

    pub fn poll_inner(&mut self, cx: &mut Context<'_>) -> AqResult<Poll<()>> {
        self.waker.set(cx.waker().clone());

        for _ in 0..32 {
            // first poll the udp driver --
            // if we don't have a udp driver we don't have an endpoint
            match std::pin::Pin::new(&mut self.udp_driver).poll(cx) {
                Poll::Pending => (),
                Poll::Ready(_) => return Err("UdpDriverEnded".into()),
            }

            let now = std::time::Instant::now();

            // order matters significantly here
            // consider carefully before changing
            let mut disp = self.poll_cmd_recv(cx)?;
            disp.merge(self.poll_udp_recv(cx, now)?);
            disp.merge(self.buffer_endpoint_transmits());
            disp.merge(self.poll_connections(cx, now)?);
            disp.merge(self.poll_udp_send(cx)?);
            disp.merge(self.poll_evt_send(cx)?);

            match disp {
                Disposition::PendOk => return Ok(Poll::Pending),
                Disposition::MoreWork => (),
            }
        }

        // we're not done, but neither are we pending...
        // need to trigger the waker, and try again
        cx.waker().wake_by_ref();
        Ok(Poll::Pending)
    }

    pub fn poll_cmd_recv(
        &mut self,
        cx: &mut Context<'_>,
    ) -> AqResult<Disposition> {
        use EndpointCmd::*;
        loop {
            match self.cmd_recv.poll_recv(cx) {
                Poll::Pending => return Ok(Disposition::PendOk),
                Poll::Ready(None) => return Err("CmdRecvEnded".into()),
                Poll::Ready(Some(cmd)) => match cmd {
                    GetLocalAddress(sender) => {
                        let recv = self.udp_send.local_addr();
                        recv.forward(sender);
                    }
                    Connect {
                        sender,
                        addr,
                        server_name,
                    } => {
                        match self.endpoint.connect(
                            (*self.client_config).clone(),
                            addr,
                            &server_name,
                        ) {
                            Err(e) => {
                                sender.send(Err(format!("{:?}", e).into()))
                            }
                            Ok((hnd, con)) => {
                                let r = self.intake_connection(hnd, con)?;
                                sender.send(Ok(r));
                            }
                        }
                    }
                },
            }
        }
    }

    pub fn poll_udp_recv(
        &mut self,
        cx: &mut Context<'_>,
        now: std::time::Instant,
    ) -> AqResult<Disposition> {
        loop {
            // since there's a chance we'll need to output an event in here
            // we cannot proceed if we have no space in our event buffer
            if self.evt_send_buf.len() >= BUF_CAP {
                // ok to pend, we'll set a waker on evt_send
                // we shouldn't need to do work if we did work in a previous
                // loop, since dependent calls follow this one
                return Ok(Disposition::PendOk);
            }

            match self.udp_recv.poll_recv(cx) {
                Poll::Pending => return Ok(Disposition::PendOk),
                Poll::Ready(None) => return Err("UdpRecvEnded".into()),
                Poll::Ready(Some(packet)) => {
                    if let Some((hnd, evt)) = self.endpoint.handle(
                        now,
                        packet.src_addr,
                        packet.dst_ip,
                        packet
                            .ecn
                            .map(|ecn| {
                                quinn_proto::EcnCodepoint::from_bits(ecn)
                            })
                            .flatten(),
                        packet.data,
                    ) {
                        use quinn_proto::DatagramEvent::*;
                        match evt {
                            ConnectionEvent(evt) => {
                                if let Some(info) =
                                    self.connections.get_mut(&hnd)
                                {
                                    info.connection.handle_event(evt);
                                }
                            }
                            NewConnection(con) => {
                                let (c, r) =
                                    self.intake_connection(hnd, con)?;
                                self.evt_send_buf
                                    .push_back(EndpointEvt::InConnection(c, r));
                            }
                        }
                    }
                }
            }
        }
    }

    pub fn buffer_endpoint_transmits(&mut self) -> Disposition {
        let mut did_work = false;

        while self.udp_send_buf.len() < BUF_CAP {
            if let Some(transmit) = self.endpoint.poll_transmit() {
                did_work = true;
                self.udp_send_buf.push_back(OutUdpPacket {
                    dst_addr: transmit.destination,
                    src_ip: transmit.src_ip,
                    segment_size: transmit.segment_size,
                    ecn: transmit.ecn.map(|ecn| ecn as u8),
                    data: transmit.contents,
                });
            }
        }

        if did_work {
            Disposition::MoreWork
        } else {
            Disposition::PendOk
        }
    }

    pub fn poll_udp_send(
        &mut self,
        cx: &mut Context<'_>,
    ) -> AqResult<Disposition> {
        let mut did_work = false;

        while !self.udp_send_buf.is_empty() {
            match self.udp_send.poll_send(cx) {
                Poll::Pending => return Ok(Disposition::PendOk),
                Poll::Ready(Err(e)) => return Err(e),
                Poll::Ready(Ok(permit)) => {
                    did_work = true;
                    permit(self.udp_send_buf.pop_front().unwrap());
                }
            }
        }

        if did_work {
            Ok(Disposition::MoreWork)
        } else {
            Ok(Disposition::PendOk)
        }
    }

    pub fn poll_evt_send(
        &mut self,
        cx: &mut Context<'_>,
    ) -> AqResult<Disposition> {
        let mut did_work = false;

        while !self.evt_send_buf.is_empty() {
            match self.evt_send.poll_send(cx) {
                Poll::Pending => return Ok(Disposition::PendOk),
                Poll::Ready(Err(e)) => return Err(e),
                Poll::Ready(Ok(permit)) => {
                    did_work = true;
                    permit(self.evt_send_buf.pop_front().unwrap());
                }
            }
        }

        if did_work {
            Ok(Disposition::MoreWork)
        } else {
            Ok(Disposition::PendOk)
        }
    }
}
