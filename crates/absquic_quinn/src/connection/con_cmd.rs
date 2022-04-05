use super::*;

pin_project_lite::pin_project! {
    pub struct ConCmdDriver {
        #[pin]
        con_cmd_recv: futures_util::stream::SelectAll<ConCmdRecv>,
        con_cmd_recv_closed: bool,
    }
}

impl ConCmdDriver {
    pub fn new(
        con_cmd_recv: futures_util::stream::SelectAll<ConCmdRecv>,
    ) -> Self {
        Self {
            con_cmd_recv,
            con_cmd_recv_closed: false,
        }
    }

    pub fn poll(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        connection: &mut quinn_proto::Connection,
        more_work: &mut bool,
        want_close: &mut bool,
    ) -> AqResult<()> {
        use futures_util::stream::Stream;

        let mut this = self.project();

        if *this.con_cmd_recv_closed {
            *want_close = true;
            return Ok(());
        }

        for _ in 0..32 {
            match this.con_cmd_recv.as_mut().poll_next(cx) {
                Poll::Pending => break,
                Poll::Ready(None) => {
                    *this.con_cmd_recv_closed = true;
                    *want_close = true;
                    return Ok(());
                }
                Poll::Ready(Some(evt)) => {
                    *more_work = true;
                    match evt {
                        ConCmd::ConEvt(evt) => {
                            connection.handle_event(evt);
                        }
                        ConCmd::ConCmd(ConnectionCmd::GetRemoteAddress(rsp)) => {
                            rsp.send(Ok(connection.remote_address()));
                        }
                        ConCmd::ConCmd(ConnectionCmd::OpenUniStream(_rsp)) => {
                            todo!()
                        }
                        ConCmd::ConCmd(ConnectionCmd::OpenBiStream(_rsp)) => {
                            todo!()
                        }
                    }
                }
            }
        }

        Ok(())
    }
}
