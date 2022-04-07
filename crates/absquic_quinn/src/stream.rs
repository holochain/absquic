use super::*;

pub struct StreamInfo {
    s_uniq: usize,
    start: std::time::Instant,
    stream_id: quinn_proto::StreamId,

    read: Option<ReadStreamBackend>,
    readable: bool,

    write: Option<WriteStreamBackend>,
    writable: bool,
}

impl StreamInfo {
    pub fn uni_in(
        stream_id: quinn_proto::StreamId,
        read: ReadStreamBackend,
    ) -> Self {
        Self {
            s_uniq: uniq(),
            start: std::time::Instant::now(),
            stream_id,
            read: Some(read),
            readable: true,
            write: None,
            writable: false,
        }
    }

    pub fn uni_out(
        stream_id: quinn_proto::StreamId,
        write: WriteStreamBackend,
    ) -> Self {
        Self {
            s_uniq: uniq(),
            start: std::time::Instant::now(),
            stream_id,
            read: None,
            readable: false,
            write: Some(write),
            writable: true,
        }
    }

    pub fn bi(
        stream_id: quinn_proto::StreamId,
        read: ReadStreamBackend,
        write: WriteStreamBackend,
    ) -> Self {
        Self {
            s_uniq: uniq(),
            start: std::time::Instant::now(),
            stream_id,
            read: Some(read),
            readable: true,
            write: Some(write),
            writable: true,
        }
    }

    pub fn set_readable(&mut self) {
        self.readable = true;
    }

    pub fn set_writable(&mut self) {
        self.writable = true;
    }

    pub fn finish(mut self) {
        self.read.take();
        self.write.take();
    }

    pub fn stop(mut self, code: quinn_proto::VarInt) {
        if let Some(rb) = self.read.take() {
            rb.stop(one_err::OneErr::new(code));
        }
        if let Some(wb) = self.write.take() {
            wb.stop(one_err::OneErr::new(code));
        }
    }

    pub fn poll(
        &mut self,
        cx: &mut Context<'_>,
        connection: &mut quinn_proto::Connection,
        remove_stream: &mut bool,
    ) -> AqResult<()> {
        let _span = tracing::error_span!(
            "s_poll",
            s_uniq = ?self.s_uniq,
        )
        .entered();

        if self.readable && self.read.is_some() {
            self.read(cx, connection)?;
        }

        if self.writable && self.write.is_some() {
            self.write(cx, connection)?;
        }

        if self.read.is_none() && self.write.is_none() {
            tracing::trace!("remove stream");
            *remove_stream = true;
        }

        Ok(())
    }

    fn read(
        &mut self,
        cx: &mut Context<'_>,
        connection: &mut quinn_proto::Connection,
    ) -> AqResult<()> {
        let mut rb = self.read.take().unwrap();

        // checking the quinn stream is more expensive,
        // let's make sure we can get a permit to send
        // on the reader backend first
        let mut permit = match rb.poll_acquire(cx) {
            Poll::Pending => {
                self.read = Some(rb);
                return Ok(());
            }
            Poll::Ready(Err(code)) => {
                if let Ok(code) = quinn_proto::VarInt::from_u64(code) {
                    let _ = connection.recv_stream(self.stream_id).stop(code);
                }
                return Ok(());
            }
            Poll::Ready(Ok(p)) => Some(p),
        };

        let mut recv_stream = connection.recv_stream(self.stream_id);
        let mut remove = false;
        let mut stop_err = None;
        let mut stop_code = None;
        match recv_stream.read(true) {
            Err(err) => {
                stop_err = Some(one_err::OneErr::new(err));
                remove = true;
            }
            Ok(mut chunks) => {
                loop {
                    if permit.is_none() {
                        match rb.poll_acquire(cx) {
                            Poll::Pending => break,
                            Poll::Ready(Err(code)) => {
                                stop_code = Some(code);
                                remove = true;
                                break;
                            }
                            Poll::Ready(Ok(p)) => permit = Some(p),
                        };
                    }
                    let p = permit.take().unwrap();
                    match chunks.next(p.max_len()) {
                        Err(quinn_proto::ReadError::Blocked) => {
                            self.readable = false;
                            break;
                        }
                        Err(err) => {
                            stop_err = Some(one_err::OneErr::new(err));
                            remove = true;
                            break;
                        }
                        Ok(None) => {
                            remove = true;
                            break;
                        }
                        Ok(Some(chunk)) => {
                            if chunk.bytes.len() > p.max_len() {
                                panic!("unexpected large chunk");
                            }
                            tracing::trace!(
                                elapsed_ms = %self.start.elapsed().as_millis(),
                                byte_count = %chunk.bytes.len(),
                            );

                            p.send(chunk.bytes);
                        }
                    }
                }

                // at the moment we *always* check transmit
                let _ = chunks.finalize();
            }
        }

        if let Some(code) = stop_code {
            if let Ok(code) = quinn_proto::VarInt::from_u64(code) {
                let _ = recv_stream.stop(code);
            }
        }

        if let Some(stop_err) = stop_err {
            rb.stop(stop_err);
        } else if !remove {
            self.read = Some(rb);
        }

        Ok(())
    }

    fn write(
        &mut self,
        cx: &mut Context<'_>,
        connection: &mut quinn_proto::Connection,
    ) -> AqResult<()> {
        let mut wb = self.write.take().unwrap();
        let mut send_stream = connection.send_stream(self.stream_id);
        let mut remove = false;
        let mut stop_err = None;
        loop {
            match wb.poll_recv(cx) {
                Poll::Pending => break,
                Poll::Ready(cmd) => match cmd {
                    WriteCmd::Data(mut cmd) => {
                        match send_stream.write(cmd.as_ref()) {
                            Ok(n) => {
                                use bytes::Buf;
                                cmd.advance(n);
                            }
                            Err(quinn_proto::WriteError::Blocked) => {
                                self.writable = false;
                                break;
                            }
                            Err(err) => {
                                stop_err = Some(one_err::OneErr::new(err));
                                remove = true;
                                break;
                            }
                        }
                    }
                    WriteCmd::Stop(error_code) => {
                        remove = true;
                        let code = if let Ok(code) =
                            quinn_proto::VarInt::from_u64(error_code)
                        {
                            code
                        } else {
                            quinn_proto::VarInt::from_u32(0)
                        };
                        if code.into_inner() == 0 {
                            let _ = send_stream.finish();
                        } else {
                            let _ = send_stream.reset(code);
                        }
                        break;
                    }
                },
            }
        }

        if let Some(stop_err) = stop_err {
            wb.stop(stop_err);
        } else if !remove {
            self.write = Some(wb);
        }

        Ok(())
    }
}
