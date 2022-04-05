use super::*;

pin_project_lite::pin_project! {
    pub struct EpEvtDriver {
        hnd: quinn_proto::ConnectionHandle,
        con_cmd_send: MultiSender<ConCmd>,
        con_cmd_send_closed: bool,
        ep_cmd_send: MultiSender<EpCmd>,
        ep_cmd_send_closed: bool,
    }
}

impl EpEvtDriver {
    pub fn new(
        hnd: quinn_proto::ConnectionHandle,
        con_cmd_send: MultiSender<ConCmd>,
        ep_cmd_send: MultiSender<EpCmd>,
    ) -> Self {
        Self {
            hnd,
            con_cmd_send,
            con_cmd_send_closed: false,
            ep_cmd_send,
            ep_cmd_send_closed: false,
        }
    }

    pub fn poll(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        connection: &mut quinn_proto::Connection,
        more_work: &mut bool,
        want_close: &mut bool,
    ) -> AqResult<()> {
        let this = self.project();

        if *this.ep_cmd_send_closed {
            *want_close = true;
            return Ok(());
        }

        let mut cmd_send = None;

        for _ in 0..32 {
            if cmd_send.is_none() && !*this.con_cmd_send_closed {
                match this.con_cmd_send.poll_acquire(cx) {
                    Poll::Pending => break,
                    Poll::Ready(Err(_)) => {
                        *this.con_cmd_send_closed = true;
                    }
                    Poll::Ready(Ok(sender)) => {
                        cmd_send = Some(sender);
                    }
                }
            }

            match this.ep_cmd_send.poll_acquire(cx) {
                Poll::Pending => return Ok(()),
                Poll::Ready(Err(_)) => {
                    tracing::debug!("con ep evt send closed");
                    *this.ep_cmd_send_closed = true;
                    *want_close = true;
                    return Ok(());
                }
                Poll::Ready(Ok(sender)) => {
                    if let Some(evt) = connection.poll_endpoint_events() {
                        if let Some(cmd_sender) = cmd_send.take() {
                            let rsp = OnceSender::new(move |evt| {
                                if let Some(evt) = evt {
                                    cmd_sender.send(ConCmd::ConEvt(evt));
                                }
                            });
                            sender.send(EpCmd::EpEvt {
                                hnd: *this.hnd,
                                evt,
                                rsp,
                            });
                        } else {
                            // the endpoint is closed and will no longer
                            // receive events... does it make sense to
                            // keep polling the connection anymore??
                        }
                    } else {
                        // for now just drop our sender.. we might decide
                        // to cache this if it's identified as inefficient
                        return Ok(());
                    }
                }
            }
        }

        // if we didn't return, there is more work
        *more_work = true;
        Ok(())
    }
}
