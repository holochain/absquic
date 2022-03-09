//! tx3_quic Endpoint Driver

use crate::connection::*;
use crate::endpoint::EndpointEvt;
use crate::types::*;
use crate::OutChan;
use crate::Tx3Result;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::collections::VecDeque;
use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::{Arc, Weak};
use std::task::Context;
use std::task::Poll;

const BATCH_FACT: usize = 8;

/// typedef for logic that can be scheduled to execute later
pub type ScheduledLogic = Arc<dyn Fn() + 'static + Send + Sync>;

/// Trait representing logic that can be scheduled to execute in the future
pub trait AsScheduled: 'static + Send {
    /// reset the schedule for this instance
    /// even if the logic has previously been executed,
    /// if the passed instance is in the future, the same logic
    /// should be scheduled to execute again at the specified instant
    /// also, don't invoke the logic within this reset_schedule invocation.
    fn reset_schedule(&mut self, at: std::time::Instant);
}

/// trait object scheduled
pub type DynScheduled = Box<dyn AsScheduled + 'static + Send>;

/// Factory for generating a scheduled instance
pub trait AsScheduledFactory: 'static + Send + Sync {
    /// get a new instance of scheduled logic that will be executed later
    fn schedule_logic(
        &self,
        at: std::time::Instant,
        logic: ScheduledLogic,
    ) -> DynScheduled;
}

/// trait object scheduled factory
pub type DynScheduledFactory =
    Arc<dyn AsScheduledFactory + 'static + Send + Sync>;

struct ConnectionInner {
    con: quinn_proto::Connection,
    sched: Option<DynScheduled>,
    timeout: bool,
    connection_evt: OutChan<ConnectionEvt>,
}

struct DriverInner {
    weak_this: Option<Weak<DriverSync>>,
    driver_waker: Option<std::task::Waker>,
    schedule_timeout: DynScheduledFactory,
    backend: Box<dyn Backend + 'static + Send>,
    endpoint: quinn_proto::Endpoint,
    capacity: usize,
    pending_send: VecDeque<Transmit>,
    pending_recv: VecDeque<InPacket>,
    connections: HashMap<quinn_proto::ConnectionHandle, ConnectionInner>,
}

#[inline(always)]
fn register_con(
    core: DriverCore,
    connections: &mut HashMap<quinn_proto::ConnectionHandle, ConnectionInner>,
    hnd: quinn_proto::ConnectionHandle,
    con: quinn_proto::Connection,
) -> (Connection, ConnectionEvtSrc) {
    let connection_evt = OutChan::new(8 /* ? */);
    let con_evt_src = ConnectionEvtSrc(connection_evt.clone());
    connections.insert(
        hnd,
        ConnectionInner {
            con,
            sched: None,
            timeout: false,
            connection_evt,
        },
    );
    let con = Connection(core, hnd);
    (con, con_evt_src)
}

#[inline(always)]
fn con_evt(
    connections: &mut HashMap<quinn_proto::ConnectionHandle, ConnectionInner>,
    hnd: quinn_proto::ConnectionHandle,
    evt: quinn_proto::ConnectionEvent,
) -> Tx3Result<()> {
    if let Some(con) = connections.get_mut(&hnd) {
        con.con.handle_event(evt);
    }
    Ok(())
}

impl DriverInner {
    fn new(
        schedule_timeout: DynScheduledFactory,
        backend: Box<dyn Backend + 'static + Send>,
        endpoint: quinn_proto::Endpoint,
    ) -> (Self, OutChan<EndpointEvt>) {
        let capacity = backend.batch_size() * BATCH_FACT;
        let endpoint_evt = OutChan::new(capacity);
        (
            Self {
                weak_this: None,
                driver_waker: None,
                schedule_timeout,
                backend,
                endpoint,
                capacity,
                pending_send: VecDeque::with_capacity(capacity),
                pending_recv: VecDeque::with_capacity(capacity),
                connections: HashMap::new(),
            },
            endpoint_evt,
        )
    }

    fn poll(
        &mut self,
        // if any function sets this to false, it's not safe to return pending
        pend_safe: &mut bool,
        cx: &mut Context<'_>,
        endpoint_evt: &mut VecDeque<EndpointEvt>,
        connection_evt: &mut VecDeque<(OutChan<ConnectionEvt>, VecDeque<ConnectionEvt>)>,
        core: DriverCore,
    ) -> Tx3Result<()> {
        // the order here is important, consider carefully before changing
        self.receive(pend_safe, cx, endpoint_evt, core)?;
        self.connections(pend_safe, connection_evt)?;
        self.transmit(pend_safe, cx)?;

        Ok(())
    }

    fn receive(
        &mut self,
        pend_safe: &mut bool,
        cx: &mut Context<'_>,
        endpoint_evt: &mut VecDeque<EndpointEvt>,
        core: DriverCore,
    ) -> Tx3Result<()> {
        let DriverInner {
            backend,
            endpoint,
            capacity,
            pending_recv,
            connections,
            ..
        } = self;

        let now = std::time::Instant::now();

        let mut recv_pending = false;

        while pending_recv.len() < *capacity {
            match backend.poll_recv(cx, pending_recv, *capacity) {
                Poll::Pending => {
                    recv_pending = true;
                    break;
                }
                Poll::Ready(Ok(count)) => {
                    if count == 0 {
                        // TODO - notify our socket closed??
                        break;
                    }
                }
                Poll::Ready(Err(e)) => return Err(e),
            }
        }

        if !recv_pending {
            *pend_safe = false;
        }

        while let Some(recv) = pending_recv.pop_front() {
            let InPacket {
                src_addr,
                dst_ip,
                ecn,
                data,
            } = recv;
            if let Some((hnd, evt)) =
                endpoint.handle(now, src_addr, dst_ip, ecn, data)
            {
                match evt {
                    quinn_proto::DatagramEvent::NewConnection(con) => {
                        let (con, evt) =
                            register_con(core.clone(), connections, hnd, con);
                        endpoint_evt
                            .push_back(EndpointEvt::Connection(con, evt));
                    }
                    quinn_proto::DatagramEvent::ConnectionEvent(evt) => {
                        con_evt(connections, hnd, evt)?;
                    }
                }
            }
        }

        Ok(())
    }

    fn connections(
        &mut self,
        pend_safe: &mut bool,
        connection_evt: &mut VecDeque<(OutChan<ConnectionEvt>, VecDeque<ConnectionEvt>)>,
    ) -> Tx3Result<()> {
        let DriverInner {
            weak_this,
            schedule_timeout,
            backend,
            endpoint,
            capacity,
            pending_send,
            connections,
            ..
        } = self;

        for (hnd, con) in connections.iter_mut() {
            let mut loop_again = true;
            let mut con_evt_out = VecDeque::new();

            while loop_again {
                loop_again = false;

                let now = std::time::Instant::now();

                let mut transmit_empty = false;

                if con.timeout {
                    con.timeout = false;
                    con.con.handle_timeout(now);
                }

                // transmit
                while pending_send.len() < *capacity {
                    match con.con.poll_transmit(now, backend.max_gso_segments())
                    {
                        None => {
                            transmit_empty = true;
                            break;
                        }
                        Some(t) => pending_send.push_back(t),
                    }
                }

                if !transmit_empty {
                    *pend_safe = false;
                }

                // timeout
                if let Some(timeout) = con.con.poll_timeout() {
                    if let Some(sched) = &mut con.sched {
                        sched.reset_schedule(timeout);
                    } else {
                        let weak_this = weak_this.as_ref().unwrap().clone();
                        let hnd = *hnd;
                        let logic: ScheduledLogic = Arc::new(move || {
                            let mut waker = None;
                            if let Some(this) = Weak::upgrade(&weak_this) {
                                let mut inner = this.inner.lock();
                                if let Some(con) =
                                    inner.connections.get_mut(&hnd)
                                {
                                    con.timeout = true;
                                }
                                waker = inner.driver_waker.take();
                            }
                            if let Some(waker) = waker {
                                waker.wake();
                            }
                        });
                        con.sched = Some(
                            schedule_timeout.schedule_logic(timeout, logic),
                        );
                    }
                }

                // endpoint_events
                while let Some(evt) = con.con.poll_endpoint_events() {
                    if let Some(evt) = endpoint.handle_event(*hnd, evt) {
                        // handle_event can cause more outputs from this con
                        loop_again = true;
                        con.con.handle_event(evt);
                    }
                }

                // poll
                while let Some(evt) = con.con.poll() {
                    match evt {
                        quinn_proto::Event::HandshakeDataReady => {
                            con_evt_out.push_back(ConnectionEvt::HandshakeDataReady);
                        }
                        quinn_proto::Event::Connected => {
                            con_evt_out.push_back(ConnectionEvt::Connected);
                        }
                        quinn_proto::Event::ConnectionLost { reason } => {
                            con_evt_out.push_back(ConnectionEvt::ConnectionLost(reason.to_string().into()));
                        }
                        quinn_proto::Event::Stream(_evt) => {
                            // TODO FIXME
                            panic!("stream evt");
                        }
                        quinn_proto::Event::DatagramReceived => {
                            // TODO FIXME
                            panic!("datagram received");
                        }
                    }
                }

                // datagram recv
                // TODO - we're just throwing these away right now
                //        so they don't fill up our memory
                while let Some(_dg) = con.con.datagrams().recv() {}
            }

            if !con_evt_out.is_empty() {
                connection_evt.push_back((con.connection_evt.clone(), con_evt_out));
            }
        }

        Ok(())
    }

    fn transmit(
        &mut self,
        pend_safe: &mut bool,
        cx: &mut Context<'_>,
    ) -> Tx3Result<()> {
        let DriverInner {
            backend,
            endpoint,
            capacity,
            pending_send,
            ..
        } = self;

        while pending_send.len() < *capacity {
            match endpoint.poll_transmit() {
                None => break,
                Some(t) => pending_send.push_back(t),
            }
        }

        let mut send_pending = false;

        while !pending_send.is_empty() {
            match backend.poll_send(cx, pending_send) {
                Poll::Pending => {
                    send_pending = true;
                    break;
                }
                Poll::Ready(Ok(_)) => (),
                Poll::Ready(Err(e)) => panic!("{:?}", e),
            }
        }

        if pending_send.is_empty() {
            // if there's nothing to send, it doesn't mean we should set
            // pend_safe to false... we need to trust that any mutators
            // called by our owner will properly wake this task so we can
            // check for additional transmit data
            return Ok(());
        }

        if !send_pending {
            *pend_safe = false;
        }

        Ok(())
    }
}

pub(crate) struct DriverSync {
    inner: Mutex<DriverInner>,
    endpoint_evt: OutChan<EndpointEvt>,
}

#[derive(Clone)]
pub(crate) struct DriverCore(pub(crate) Arc<DriverSync>);

impl DriverCore {
    pub(crate) fn new(
        schedule_timeout: DynScheduledFactory,
        backend: Box<dyn Backend + 'static + Send>,
        endpoint: quinn_proto::Endpoint,
    ) -> (Self, OutChan<EndpointEvt>) {
        let (inner, endpoint_evt) =
            DriverInner::new(schedule_timeout, backend, endpoint);
        let inner = Mutex::new(inner);
        let out = Self(Arc::new(DriverSync {
            inner,
            endpoint_evt: endpoint_evt.clone(),
        }));
        let weak_this = Arc::downgrade(&out.0);
        out.0.inner.lock().weak_this = Some(weak_this);
        (out, endpoint_evt)
    }

    pub(crate) fn local_addr(&self) -> Tx3Result<SocketAddr> {
        self.0.inner.lock().backend.local_addr()
    }

    pub(crate) fn connect(
        &self,
        config: quinn_proto::ClientConfig,
        remote: SocketAddr,
        server_name: &str,
    ) -> Tx3Result<(Connection, ConnectionEvtSrc)> {
        let (driver_waker, con, evt) = {
            let mut inner = self.0.inner.lock();
            let (hnd, con) = inner
                .endpoint
                .connect(config, remote, server_name)
                .map_err(one_err::OneErr::new)?;
            let (con, evt) =
                register_con(self.clone(), &mut inner.connections, hnd, con);
            (inner.driver_waker.take(), con, evt)
        };
        // release the lock before waking
        if let Some(waker) = driver_waker {
            waker.wake();
        }
        Ok((con, evt))
    }

    pub(crate) fn con_remote_addr(
        &self,
        hnd: quinn_proto::ConnectionHandle,
    ) -> Tx3Result<SocketAddr> {
        match self.0.inner.lock().connections.get(&hnd) {
            Some(c) => Ok(c.con.remote_address()),
            None => Err("InvalidConnectionHandle".into()),
        }
    }

    // -- private -- //

    fn poll(
        self,
        cx: &mut Context<'_>,
        endpoint_evt: &mut VecDeque<EndpointEvt>,
        connection_evt: &mut VecDeque<(OutChan<ConnectionEvt>, VecDeque<ConnectionEvt>)>,
        core: DriverCore,
    ) -> Poll<()> {
        let mut pend_safe = true;

        // this num is fairly arbitrary, more benchmarks needed
        const LOOP_COUNT: usize = 100;

        for _ in 0..LOOP_COUNT {
            // trust in the parking_lot optimizations that most often keep a
            // mutex ready when a single thread re-requests it immediately
            // and on average fairly release it if we're taking too much cpu
            let mut inner = self.0.inner.lock();

            if let Err(e) =
                inner.poll(&mut pend_safe, cx, endpoint_evt, connection_evt, core.clone())
            {
                // TODO - tracing error?? then exit ready??
                panic!("{:?}", e);
            }

            if pend_safe {
                inner.driver_waker = Some(cx.waker().clone());
                break;
            }

            pend_safe = true;
        }

        // trigger endpoint events *after* we release the lock
        // Q: do we want to do this within the loop, or once at the end?
        self.0.endpoint_evt.push_back(endpoint_evt);

        // trigger connection events *after* we relase the lock
        // Q: do we want to do this within the loop, or once at the end?
        for (connection_evt, mut evt_list) in connection_evt.drain(..) {
            connection_evt.push_back(&mut evt_list);
        }

        if pend_safe {
            return Poll::Pending;
        }

        // we don't want to starve other tasks, but we're not done either,
        // it's not safe to just return pending, we might not be woken,
        // so trigger our waker, then return Pending
        cx.waker().wake_by_ref();
        Poll::Pending
    }
}

/// Quic Endpoint Driver
#[must_use = "futures do nothing unless you `.await` or poll them"]
pub struct EndpointDriver {
    weak_this: Weak<DriverSync>,
    endpoint_evt: VecDeque<EndpointEvt>,
    connection_evt: VecDeque<(OutChan<ConnectionEvt>, VecDeque<ConnectionEvt>)>,
}

impl EndpointDriver {
    pub(crate) fn new(core: &DriverCore) -> Self {
        let weak_this = Arc::downgrade(&core.0);
        Self {
            weak_this,
            endpoint_evt: VecDeque::new(),
            connection_evt: VecDeque::new(),
        }
    }
}

impl Future for EndpointDriver {
    type Output = ();

    fn poll(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Self::Output> {
        let core = match Weak::upgrade(&self.weak_this) {
            Some(core) => DriverCore(core),
            None => return Poll::Ready(()),
        };
        let core2 = core.clone();
        let EndpointDriver {
            endpoint_evt,
            connection_evt,
            ..
        } = &mut *self;
        core.poll(cx, endpoint_evt, connection_evt, core2)
    }
}
