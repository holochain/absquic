use super::*;

mod con_cmd;
pub(crate) use con_cmd::*;

mod transmit;
pub(crate) use transmit::*;

mod ep_evt;
pub(crate) use ep_evt::*;

pub enum ConCmd {
    ConEvt(quinn_proto::ConnectionEvent),
    ConCmd(ConnectionCmd),
}

type ConCmdRecv = futures_util::stream::BoxStream<'static, ConCmd>;

pin_project_lite::pin_project! {
    pub struct ConnectionDriver<Runtime: AsyncRuntime> {
        connection: quinn_proto::Connection,

        #[pin]
        con_cmd: ConCmdDriver,

        #[pin]
        transmit: ConTransmitDriver,

        #[pin]
        ep_evt: EpEvtDriver,

        _p: std::marker::PhantomData<Runtime>,
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
        let (_con_evt_send, con_evt_recv) = Runtime::channel(16);
        let (cmd_send, cmd_recv) = Runtime::channel(16);
        let con = construct_connection::<Runtime>(cmd_send);
        let (con_cmd_send, con_cmd_recv) = Runtime::channel(16);

        let con_cmd_recv = futures_util::stream::select_all(vec![
            con_cmd_recv.boxed(),
            cmd_recv.map(|cmd| ConCmd::ConCmd(cmd)).boxed(),
        ]);

        let con_cmd = ConCmdDriver::new(con_cmd_recv);

        let transmit = ConTransmitDriver::new(udp_packet_send);

        let ep_evt = EpEvtDriver::new(hnd, con_cmd_send.clone(), ep_cmd_send);

        Runtime::spawn(Self {
            connection,

            con_cmd,
            transmit,
            ep_evt,

            _p: std::marker::PhantomData,
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

            let now = std::time::Instant::now();

            let mut con_cmd_want_close = false;
            this.con_cmd.as_mut().poll(
                cx,
                this.connection,
                &mut more_work,
                &mut con_cmd_want_close,
            )?;

            let mut transmit_want_close = false;
            this.transmit.as_mut().poll(
                cx,
                now,
                this.connection,
                &mut more_work,
                &mut transmit_want_close,
            )?;

            // TODO poll_timeout()

            let mut ep_evt_want_close = false;
            this.ep_evt.as_mut().poll(
                cx,
                this.connection,
                &mut more_work,
                &mut ep_evt_want_close,
            )?;

            if con_cmd_want_close && transmit_want_close && ep_evt_want_close {
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
