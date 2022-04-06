use super::*;

mod con_cmd;
pub(crate) use con_cmd::*;

mod transmit;
pub(crate) use transmit::*;

mod ep_evt;
pub(crate) use ep_evt::*;

mod con_poll;
pub(crate) use con_poll::*;

pub enum ConCmd {
    ConEvt(quinn_proto::ConnectionEvent),
    ConCmd(ConnectionCmd),
}

type ConCmdRecv = futures_util::stream::BoxStream<'static, ConCmd>;

pub type StreamMap = HashMap<quinn_proto::StreamId, StreamInfo>;

pin_project_lite::pin_project! {
    pub struct ConnectionDriver<Runtime: AsyncRuntime> {
        connection: quinn_proto::Connection,

        inbound_datagrams: bool,
        inbound_uni: bool,
        inbound_bi: bool,
        outbound_uni: bool,
        outbound_bi: bool,
        streams: StreamMap,

        #[pin]
        con_cmd: ConCmdDriver<Runtime>,

        #[pin]
        transmit: ConTransmitDriver,

        timer: Timer<Runtime>,

        #[pin]
        ep_evt: EpEvtDriver,

        #[pin]
        con_poll: ConPollDriver<Runtime>,
    }
}

impl<Runtime: AsyncRuntime> Future for ConnectionDriver<Runtime> {
    type Output = AqResult<()>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        if let Err(err) = self.poll_inner(cx) {
            tracing::error!(?err);
            Poll::Ready(Err(err))
        } else {
            Poll::Pending
        }
    }
}

impl<Runtime: AsyncRuntime> ConnectionDriver<Runtime> {
    pub fn spawn(
        hnd: quinn_proto::ConnectionHandle,
        connection: quinn_proto::Connection,
        ep_cmd_send: MultiSender<EpCmd>,
        udp_packet_send: MultiSender<OutUdpPacket>,
    ) -> (
        Connection,
        MultiSender<ConCmd>,
        MultiReceiver<ConnectionEvt>,
    ) {
        let (con_evt_send, con_evt_recv) = Runtime::channel(16);
        let (cmd_send, cmd_recv) = Runtime::channel(16);
        let con = construct_connection::<Runtime>(cmd_send);
        let (con_cmd_send, con_cmd_recv) = Runtime::channel(16);

        let con_cmd_recv = futures_util::stream::select_all(vec![
            con_cmd_recv.boxed(),
            cmd_recv.map(|cmd| ConCmd::ConCmd(cmd)).boxed(),
        ]);

        let con_cmd = ConCmdDriver::new(con_cmd_recv);

        let transmit = ConTransmitDriver::new(udp_packet_send);

        let timer = Timer::new();

        let ep_evt = EpEvtDriver::new(hnd, con_cmd_send.clone(), ep_cmd_send);

        let con_poll = ConPollDriver::new(con_evt_send);

        Runtime::spawn(Self {
            connection,

            inbound_datagrams: true,
            inbound_uni: true,
            inbound_bi: true,
            outbound_uni: true,
            outbound_bi: true,
            streams: StreamMap::new(),

            con_cmd,
            transmit,
            timer,
            ep_evt,
            con_poll,
        });

        (con, con_cmd_send, con_evt_recv)
    }

    pub fn poll_inner(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> AqResult<()> {
        let mut this = self.project();

        let mut more_work;

        let start = std::time::Instant::now();
        loop {
            more_work = false;

            // quinn_proto::Connection wants us to use the following order:
            //
            // A) simple getters (can be called whenever)
            // B) network handlers (handled at the endpoint level)
            // C) commands / stream io
            // D) - 1) poll_transmit
            //    - 2) poll_timeout
            //    - 3) poll_endpoint_events
            //    - 4) poll

            let now = if this.timer.should_trigger(cx) {
                let now = std::time::Instant::now();
                this.connection.handle_timeout(now);
                now
            } else {
                std::time::Instant::now()
            };

            // C - commands
            let mut con_cmd_want_close = false;
            this.con_cmd.as_mut().poll(
                cx,
                this.streams,
                this.connection,
                this.outbound_uni,
                this.outbound_bi,
                &mut more_work,
                &mut con_cmd_want_close,
            )?;

            // C - stream io
            let mut rm_stream = Vec::new();
            for (id, info) in this.streams.iter_mut() {
                let mut rm = false;
                info.poll(cx, this.connection, &mut rm)?;
                if rm {
                    rm_stream.push(*id);
                }
            }
            for id in rm_stream {
                this.streams.remove(&id);
            }

            // D.1 - poll_transmit
            let mut transmit_want_close = false;
            this.transmit.as_mut().poll(
                cx,
                now,
                this.connection,
                &mut more_work,
                &mut transmit_want_close,
            )?;

            // D.2 - poll_timeout
            if let Some(timeout) = this.connection.poll_timeout() {
                this.timer.set_time(cx, timeout);
            }

            // D.3 - poll_endpoint_events
            let mut ep_evt_want_close = false;
            this.ep_evt.as_mut().poll(
                cx,
                this.connection,
                &mut more_work,
                &mut ep_evt_want_close,
            )?;

            // D.4 - poll
            let mut con_poll_want_close = false;
            this.con_poll.as_mut().poll(
                cx,
                this.inbound_datagrams,
                this.inbound_uni,
                this.inbound_bi,
                this.outbound_uni,
                this.outbound_bi,
                this.streams,
                this.connection,
                &mut more_work,
                &mut con_poll_want_close,
            )?;

            if con_cmd_want_close
                && transmit_want_close
                && ep_evt_want_close
                && con_poll_want_close
            {
                return Err("QuinnConDriverClosing".into());
            }

            if !more_work || start.elapsed().as_millis() >= 10 {
                break;
            }
        }

        tracing::trace!(elapsed_ms = %start.elapsed().as_millis(), "con poll");

        if more_work {
            cx.waker().wake_by_ref();
        }

        Ok(())
    }
}

struct Timer<Runtime: AsyncRuntime> {
    inner: Option<(std::time::Instant, AqFut<'static, ()>)>,
    _p: std::marker::PhantomData<Runtime>,
}

impl<Runtime: AsyncRuntime> Timer<Runtime> {
    pub fn new() -> Self {
        Timer {
            inner: None,
            _p: std::marker::PhantomData,
        }
    }

    pub fn should_trigger(&mut self, cx: &mut Context<'_>) -> bool {
        match self.inner.take() {
            None => false,
            Some((time, mut fut)) => match Pin::new(&mut fut).poll(cx) {
                Poll::Pending => {
                    self.inner = Some((time, fut));
                    false
                }
                Poll::Ready(()) => true,
            },
        }
    }

    pub fn set_time(&mut self, cx: &mut Context<'_>, time: std::time::Instant) {
        let (time, mut fut) = match self.inner.take() {
            None => (time, Runtime::sleep(time)),
            Some((cur_time, cur_fut)) => {
                if time < cur_time {
                    (time, Runtime::sleep(time))
                } else {
                    (cur_time, cur_fut)
                }
            }
        };

        match Pin::new(&mut fut).poll(cx) {
            // this is the "correct" response
            // which registers our waker with the timeout future
            Poll::Pending => self.inner = Some((time, fut)),
            // woops, we should manually trigger right away
            // but we still need a stub future to be polled
            Poll::Ready(()) => {
                cx.waker().wake_by_ref();
                self.inner = Some((time, AqFut::new(async {})))
            }
        }
    }
}
