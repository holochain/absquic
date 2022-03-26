//! absquic_core stream types

use crate::util::*;
use crate::AqResult;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic;
use std::sync::Arc;
use std::task::Context;
use std::task::Poll;

/// types only relevant when implementing a quic state machine backend
pub mod backend {
    use super::*;

    struct CounterInner {
        max: usize,
        counter: atomic::AtomicUsize,
        waker: parking_lot::Mutex<Option<std::task::Waker>>,
    }

    impl CounterInner {
        fn wake(&self) {
            if let Some(waker) = self.waker.lock().take() {
                waker.wake();
            }
        }
    }

    pub(crate) struct Counter(Arc<CounterInner>);

    impl Counter {
        fn new(max: usize) -> Self {
            Self(Arc::new(CounterInner {
                max,
                counter: atomic::AtomicUsize::new(0),
                waker: parking_lot::Mutex::new(None),
            }))
        }

        pub(crate) fn reserve(
            &self,
            cx: &mut Context<'_>,
        ) -> Option<CounterAddPermit> {
            let cur = self.0.counter.swap(self.0.max, atomic::Ordering::AcqRel);
            if cur >= self.0.max {
                *self.0.waker.lock() = Some(cx.waker().clone());
                None
            } else {
                let amount = self.0.max - cur;
                Some(CounterAddPermit(self.0.clone(), amount))
            }
        }
    }

    pub(crate) struct CounterAddPermit(Arc<CounterInner>, usize);

    impl Drop for CounterAddPermit {
        fn drop(&mut self) {
            self.0.counter.fetch_sub(self.1, atomic::Ordering::AcqRel);
            self.0.wake();
        }
    }

    impl CounterAddPermit {
        pub(crate) fn len(&self) -> usize {
            self.1
        }

        pub(crate) fn downgrade_to(&mut self, new_amount: usize) {
            if new_amount == self.1 {
                return;
            }

            if new_amount < 1 || new_amount > self.1 {
                panic!("invalid target amount");
            }

            let sub = self.1 - new_amount;
            self.1 = new_amount;
            self.0.counter.fetch_sub(sub, atomic::Ordering::AcqRel);
            self.0.wake();
        }
    }

    /// The max length of a bytes authorized for a ReadStreamBackend push
    pub type ReadMaxSize = usize;

    /// A callback allowing data to be pushed into a ReadStreamBackend
    /// if it returns Err(u64) the u64 is the stream stop error code
    /// (0 may indicate it was dropped without explicit code)
    pub type ReadCb =
        Box<dyn FnOnce(bytes::Bytes) -> Result<(), u64> + 'static + Send>;

    /// The backend of a ReadStream, allows publishing data to the api user
    pub struct ReadStreamBackend {
        counter: Counter,
        send: tokio::sync::mpsc::UnboundedSender<
            AqResult<(bytes::Bytes, CounterAddPermit)>,
        >,
        error_code: Arc<atomic::AtomicU64>,
    }

    impl ReadStreamBackend {
        /// Shutdown this read stream with given error
        pub fn close_with_error(self, err: one_err::OneErr) {
            let _ = self.send.send(Err(err));
        }

        /// Request to push data into the read stream
        /// if it returns Err(u64) the u64 is the stream stop error code
        /// (0 may indicate it was dropped without explicit code)
        pub fn poll_supply_data(
            &mut self,
            cx: &mut Context<'_>,
        ) -> Poll<Result<(ReadMaxSize, ReadCb), u64>> {
            if self.send.is_closed() {
                return Poll::Ready(Err(self
                    .error_code
                    .load(atomic::Ordering::Acquire)));
            }
            if let Some(mut permit) = self.counter.reserve(cx) {
                let sender = self.send.clone();
                let max_size = permit.len();
                let error_code = self.error_code.clone();
                let read_cb: ReadCb = Box::new(move |mut b| {
                    if b.len() > max_size {
                        b = b.split_to(max_size);
                    } else if b.len() < max_size {
                        permit.downgrade_to(b.len());
                    }
                    match sender.send(Ok((b, permit))) {
                        Ok(_) => Ok(()),
                        Err(_) => {
                            Err(error_code.load(atomic::Ordering::Acquire))
                        }
                    }
                });
                Poll::Ready(Ok((max_size, read_cb)))
            } else {
                Poll::Pending
            }
        }

        /// Request to push data into the read stream
        /// if it returns Err(u64) the u64 is the stream stop error code
        /// (0 may indicate it was dropped without explicit code)
        pub async fn supply_data(
            &mut self,
        ) -> Result<(ReadMaxSize, ReadCb), u64> {
            struct X<'lt>(&'lt mut ReadStreamBackend);

            impl<'lt> Future for X<'lt> {
                type Output = Result<(ReadMaxSize, ReadCb), u64>;

                fn poll(
                    mut self: Pin<&mut Self>,
                    cx: &mut Context<'_>,
                ) -> Poll<Self::Output> {
                    self.0.poll_supply_data(cx)
                }
            }

            X(self).await
        }
    }

    /// Construct a new read stream backend and frontend pair
    pub fn read_stream_pair(
        buf_size: usize,
    ) -> (ReadStreamBackend, ReadStream) {
        let error_code = Arc::new(atomic::AtomicU64::new(0));
        let counter = Counter::new(buf_size);
        // unbounded ok, because we're using Counter to set a bound
        // on the outstanding byte count sent over the channel
        let (send, recv) = tokio::sync::mpsc::unbounded_channel();
        (
            ReadStreamBackend {
                counter,
                send,
                error_code: error_code.clone(),
            },
            ReadStream {
                recv,
                buf: None,
                error_code,
            },
        )
    }

    pub(crate) enum WriteCmdInner {
        Data(bytes::Bytes, CounterAddPermit),
        Stop(u64, Option<OneShotSender<()>>),
    }

    /// WriteCmdData
    pub struct WriteCmdData<'lt> {
        r: &'lt mut WriteStreamBackend,
        data: Option<bytes::Bytes>,
        permit: Option<CounterAddPermit>,
    }

    impl Drop for WriteCmdData<'_> {
        fn drop(&mut self) {
            let b = self.data.take().unwrap();
            let mut p = self.permit.take().unwrap();
            if b.len() > 0 {
                if b.len() < p.len() {
                    p.downgrade_to(b.len());
                }
                self.r.buf = Some(WriteCmdInner::Data(b, p));
            }
        }
    }

    impl<'lt> std::ops::Deref for WriteCmdData<'lt> {
        type Target = bytes::Bytes;

        fn deref(&self) -> &Self::Target {
            self.data.as_ref().unwrap()
        }
    }

    impl<'lt> std::ops::DerefMut for WriteCmdData<'lt> {
        fn deref_mut(&mut self) -> &mut Self::Target {
            self.data.as_mut().unwrap()
        }
    }

    impl<'lt> AsRef<bytes::Bytes> for WriteCmdData<'lt> {
        fn as_ref(&self) -> &bytes::Bytes {
            self.data.as_ref().unwrap()
        }
    }

    impl<'lt> AsMut<bytes::Bytes> for WriteCmdData<'lt> {
        fn as_mut(&mut self) -> &mut bytes::Bytes {
            self.data.as_mut().unwrap()
        }
    }

    impl<'lt> std::borrow::Borrow<bytes::Bytes> for WriteCmdData<'lt> {
        fn borrow(&self) -> &bytes::Bytes {
            self.data.as_ref().unwrap()
        }
    }

    impl<'lt> std::borrow::BorrowMut<bytes::Bytes> for WriteCmdData<'lt> {
        fn borrow_mut(&mut self) -> &mut bytes::Bytes {
            self.data.as_mut().unwrap()
        }
    }

    /// Incoming command from the frontend write stream
    pub enum WriteCmd<'lt> {
        /// Data sent over the write channel
        /// this guard can dereference to a `&mut bytes::Bytes`
        /// you call pull off as much or little data as you like
        /// when the guard is dropped, the sender side will be notified
        /// of any additional space available from bytes pulled off.
        Data(WriteCmdData<'lt>),

        /// This write channel has been stopped with error_code
        /// no more data will be forthcoming. If error_code is 0,
        /// it may indicate the fontend side was dropped.
        Stop(u64),
    }

    /// The backend of a write stream, allows collecting the written data
    pub struct WriteStreamBackend {
        recv: tokio::sync::mpsc::UnboundedReceiver<WriteCmdInner>,
        buf: Option<WriteCmdInner>,
        error_code: u64,
    }

    impl WriteStreamBackend {
        fn poll_recv_inner(
            &mut self,
            cx: &mut Context<'_>,
        ) -> Poll<WriteCmdInner> {
            match if let Some(cmd) = self.buf.take() {
                cmd
            } else {
                match self.recv.poll_recv(cx) {
                    Poll::Pending => return Poll::Pending,
                    Poll::Ready(None) => {
                        return Poll::Ready(WriteCmdInner::Stop(
                            self.error_code,
                            None,
                        ))
                    }
                    Poll::Ready(Some(cmd)) => cmd,
                }
            } {
                WriteCmdInner::Data(b, _p) if b.len() == 0 => {
                    // tail recurse
                    self.poll_recv_inner(cx)
                }
                WriteCmdInner::Stop(error_code, send) => {
                    self.error_code = error_code;
                    if let Some(send) = send {
                        send.send(Ok(()));
                    }
                    Poll::Ready(WriteCmdInner::Stop(error_code, None))
                }
                cmd => Poll::Ready(cmd),
            }
        }

        // this can't be part of poll_recv_inner, because recv
        // has to use a future with a mutable reference...
        // call this after you get a Ready(cmd) from poll_recv_inner
        fn cmd_unchecked(&mut self, cmd: WriteCmdInner) -> WriteCmd<'_> {
            match cmd {
                WriteCmdInner::Data(b, p) => WriteCmd::Data(WriteCmdData {
                    r: self,
                    data: Some(b),
                    permit: Some(p),
                }),
                WriteCmdInner::Stop(error_code, _) => {
                    WriteCmd::Stop(error_code)
                }
            }
        }

        /// Receive data written by the frontend side of this write stream.
        pub fn poll_recv(
            &mut self,
            cx: &mut Context<'_>,
        ) -> Poll<WriteCmd<'_>> {
            match self.poll_recv_inner(cx) {
                Poll::Pending => Poll::Pending,
                Poll::Ready(cmd) => Poll::Ready(self.cmd_unchecked(cmd)),
            }
        }

        /// Receive data written by the frontend side of this write stream.
        pub async fn recv(&mut self) -> WriteCmd<'_> {
            struct X<'lt>(&'lt mut WriteStreamBackend);

            impl<'lt> Future for X<'lt> {
                type Output = WriteCmdInner;

                fn poll(
                    mut self: Pin<&mut Self>,
                    cx: &mut Context<'_>,
                ) -> Poll<Self::Output> {
                    self.0.poll_recv_inner(cx)
                }
            }

            let cmd = X(self).await;
            self.cmd_unchecked(cmd)
        }
    }

    /// Construct a new write stream backend and frontend pair
    pub fn write_stream_pair(
        buf_size: usize,
    ) -> (WriteStreamBackend, WriteStream) {
        let counter = Counter::new(buf_size);
        // unbounded ok, because we're using Counter to set a bound
        // on the outstanding byte count sent over the channel
        let (send, recv) = tokio::sync::mpsc::unbounded_channel();
        (
            WriteStreamBackend {
                recv,
                buf: None,
                error_code: 0,
            },
            WriteStream { counter, send },
        )
    }
}

use backend::*;

/// Quic read stream
pub struct ReadStream {
    recv: tokio::sync::mpsc::UnboundedReceiver<
        AqResult<(bytes::Bytes, CounterAddPermit)>,
    >,
    buf: Option<(bytes::Bytes, CounterAddPermit)>,
    error_code: Arc<atomic::AtomicU64>,
}

impl ReadStream {
    /// Read a chunk of data from the stream
    pub fn poll_read_chunk(
        &mut self,
        cx: &mut Context<'_>,
        max_bytes: usize,
    ) -> Poll<Option<AqResult<bytes::Bytes>>> {
        if self.buf.is_none() {
            match self.recv.poll_recv(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(None) => return Poll::Ready(None),
                Poll::Ready(Some(Err(e))) => return Poll::Ready(Some(Err(e))),
                Poll::Ready(Some(Ok((b, p)))) => self.buf = Some((b, p)),
            }
        }

        if max_bytes == 0 {
            return Poll::Ready(Some(Ok(bytes::Bytes::new())));
        }

        let (mut b, mut p) = self.buf.take().unwrap();

        if b.len() <= max_bytes {
            Poll::Ready(Some(Ok(b)))
        } else {
            let out = b.split_to(max_bytes);
            p.downgrade_to(max_bytes);
            self.buf = Some((b, p));
            Poll::Ready(Some(Ok(out)))
        }
    }

    /// Read a chunk of data from the stream
    pub async fn read_chunk(
        &mut self,
        max_bytes: usize,
    ) -> Option<AqResult<bytes::Bytes>> {
        struct X<'lt>(&'lt mut ReadStream, usize);

        impl<'lt> Future for X<'lt> {
            type Output = Option<AqResult<bytes::Bytes>>;

            fn poll(
                mut self: Pin<&mut Self>,
                cx: &mut Context<'_>,
            ) -> Poll<Self::Output> {
                let max_bytes = self.1;
                self.0.poll_read_chunk(cx, max_bytes)
            }
        }

        X(self, max_bytes).await
    }

    /// Cancel the stream with given error code
    pub fn stop(self, error_code: u64) {
        self.error_code.store(error_code, atomic::Ordering::Release);
    }
}

/// Quic write stream
pub struct WriteStream {
    counter: Counter,
    send: tokio::sync::mpsc::UnboundedSender<WriteCmdInner>,
}

impl WriteStream {
    /// Write data to the stream
    pub fn poll_write_chunk(
        &mut self,
        cx: &mut Context<'_>,
        chunk: &mut bytes::Bytes,
    ) -> Poll<AqResult<()>> {
        match self.counter.reserve(cx) {
            None => Poll::Pending,
            Some(mut permit) => {
                let b = if chunk.len() >= permit.len() {
                    chunk.split_to(permit.len())
                } else {
                    let len = chunk.len();
                    permit.downgrade_to(len);
                    chunk.split_to(len)
                };
                self.send
                    .send(WriteCmdInner::Data(b, permit))
                    .map_err(|_| one_err::OneErr::new("ChannelClosed"))?;
                Poll::Ready(Ok(()))
            }
        }
    }

    /// Write data to the stream
    pub async fn write_chunk(
        &mut self,
        chunk: &mut bytes::Bytes,
    ) -> AqResult<()> {
        struct X<'lt1, 'lt2>(&'lt1 mut WriteStream, &'lt2 mut bytes::Bytes);

        impl<'lt1, 'lt2> Future for X<'lt1, 'lt2> {
            type Output = AqResult<()>;

            fn poll(
                mut self: Pin<&mut Self>,
                cx: &mut Context<'_>,
            ) -> Poll<Self::Output> {
                let X(s, b) = &mut *self;
                s.poll_write_chunk(cx, b)
            }
        }

        X(self, chunk).await
    }

    /// Write data completely to the stream
    pub async fn write_chunk_all(
        &mut self,
        chunk: &mut bytes::Bytes,
    ) -> AqResult<()> {
        while !chunk.is_empty() {
            self.write_chunk(chunk).await?;
        }
        Ok(())
    }

    /// Stop this write channel with error_code
    pub fn stop(self, error_code: u64) -> OneShotReceiver<()> {
        OneShotReceiver::new(async move {
            let (s, r) = one_shot_channel();
            self.send
                .send(WriteCmdInner::Stop(error_code, Some(s)))
                .map_err(|_| one_err::OneErr::new("ChannelClosed"))?;
            Ok(OneShotKind::Forward(r.into_inner()))
        })
    }
}
