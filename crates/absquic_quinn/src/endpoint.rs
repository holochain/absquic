use super::*;

mod command;
pub(crate) use command::*;

mod udp_in;
pub(crate) use udp_in::*;

mod transmit;
pub(crate) use transmit::*;

pub enum EpCmd {
    EpEvt {
        hnd: quinn_proto::ConnectionHandle,
        evt: quinn_proto::EndpointEvent,
        rsp: OnceSender<Option<quinn_proto::ConnectionEvent>>,
    },
    EpCmd(EndpointCmd),
}

type EpCmdRecv = futures_util::stream::BoxStream<'static, EpCmd>;
type ConMap = HashMap<quinn_proto::ConnectionHandle, MultiSenderPoll<ConCmd>>;

pin_project_lite::pin_project! {
    pub struct EndpointDriver<Runtime: AsyncRuntime> {
        ep_uniq: usize,
        endpoint: quinn_proto::Endpoint,
        connections: ConMap,

        #[pin]
        command: CommandDriver<Runtime>,

        #[pin]
        udp_in: UdpInDriver<Runtime>,

        #[pin]
        transmit: TransmitDriver,
    }
}

impl<Runtime: AsyncRuntime> Future for EndpointDriver<Runtime> {
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

impl<Runtime: AsyncRuntime> EndpointDriver<Runtime> {
    pub fn spawn(
        max_gso_provider: MaxGsoProvider,
        client_config: Arc<quinn_proto::ClientConfig>,
        endpoint: quinn_proto::Endpoint,
        udp_cmd_send: MultiSender<UdpBackendCmd>,
        udp_packet_send: MultiSender<OutUdpPacket>,
        udp_packet_recv: MultiReceiver<UdpBackendEvt>,
    ) -> (MultiSender<EndpointCmd>, MultiReceiver<EndpointEvt>) {
        let (ep_cmd_send, ep_cmd_recv) = Runtime::channel(CHAN_CAP);
        let (evt_send, evt_recv) = Runtime::channel(CHAN_CAP);
        let (cmd_send, cmd_recv) = Runtime::channel(CHAN_CAP);

        let ep_cmd_recv = futures_util::stream::select_all(vec![
            ep_cmd_recv.boxed(),
            cmd_recv.map(EpCmd::EpCmd).boxed(),
        ]);

        let ep_uniq = uniq();

        let command = CommandDriver::new(
            ep_uniq,
            max_gso_provider.clone(),
            client_config,
            ep_cmd_send.clone(),
            udp_packet_send.clone(),
            ep_cmd_recv,
            udp_cmd_send,
        );

        let udp_in = UdpInDriver::new(
            ep_uniq,
            max_gso_provider,
            ep_cmd_send,
            udp_packet_send.clone(),
            evt_send,
            udp_packet_recv,
        );

        let transmit = TransmitDriver::new(udp_packet_send);

        Runtime::spawn(Self {
            ep_uniq,
            endpoint,
            connections: HashMap::new(),

            command,
            udp_in,
            transmit,
        });

        (cmd_send, evt_recv)
    }

    pub fn poll_inner(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> AqResult<()> {
        let mut this = self.project();

        let _span =
            tracing::error_span!("ep_poll", ep_uniq = ?this.ep_uniq).entered();

        let mut more_work;

        let start = std::time::Instant::now();
        let mut elapsed_ms;
        loop {
            more_work = false;

            let mut cmd_want_close = false;
            this.command.as_mut().poll(
                cx,
                this.endpoint,
                this.connections,
                &mut more_work,
                &mut cmd_want_close,
            )?;

            let mut udp_in_want_close = false;
            this.udp_in.as_mut().poll(
                cx,
                this.endpoint,
                this.connections,
                &mut more_work,
                &mut udp_in_want_close,
            )?;

            let mut transmit_want_close = false;
            this.transmit.as_mut().poll(
                cx,
                this.endpoint,
                &mut more_work,
                &mut transmit_want_close,
            )?;

            if cmd_want_close && udp_in_want_close && transmit_want_close {
                return Err("QuinnEpDriverClosing".into());
            }

            elapsed_ms = start.elapsed().as_millis();
            if !more_work || elapsed_ms >= 10 {
                break;
            }
        }

        if elapsed_ms >= 20 {
            tracing::warn!(%elapsed_ms, "long ep poll");
        }

        if more_work {
            cx.waker().wake_by_ref();
        }

        Ok(())
    }
}
