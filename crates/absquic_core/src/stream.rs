#![allow(clippy::type_complexity)]
#![allow(clippy::comparison_chain)]
//! Absquic_core stream types

use crate::runtime::*;
use crate::sync::atomic;
use crate::sync::Arc;
use crate::sync::Mutex;
use crate::*;
use std::future::Future;
use std::pin::Pin;
use std::task::Context;
use std::task::Poll;

pub mod buf_list;

/// types only relevant when implementing a quic state machine backend
pub mod backend {
    use super::*;

    struct CounterInner {
        max: usize,
        inner: Mutex<(usize, Option<std::task::Waker>)>,
    }

    impl CounterInner {
        fn reserve(&self, cx: &mut Context<'_>) -> Option<usize> {
            let max = self.max;
            let mut inner = self.inner.lock();
            let amount = max - inner.0;
            if amount > 0 {
                inner.0 = max;
                Some(amount)
            } else {
                inner.1 = Some(cx.waker().clone());
                None
            }
        }

        fn restore(&self, amount: usize) {
            if let Some(waker) = {
                let mut inner = self.inner.lock();
                inner.0 -= amount;
                inner.1.take()
            } {
                waker.wake();
            }
        }
    }

    pub(crate) struct Counter(Arc<CounterInner>);

    impl Counter {
        fn new(max: usize) -> Self {
            Self(Arc::new(CounterInner {
                max,
                inner: Mutex::new((0, None)),
            }))
        }

        pub(crate) fn reserve(
            &self,
            cx: &mut Context<'_>,
        ) -> Option<CounterAddPermit> {
            self.0
                .reserve(cx)
                .map(|amount| CounterAddPermit(self.0.clone(), amount))
        }
    }

    pub(crate) struct CounterAddPermit(Arc<CounterInner>, usize);

    impl Drop for CounterAddPermit {
        fn drop(&mut self) {
            self.0.restore(self.1);
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
            self.0.restore(sub);
        }
    }

    /// Permit allowing sending of data to the front-end ReadStream
    pub struct ReadSendPermit {
        sender: OnceSender<(bytes::Bytes, CounterAddPermit)>,
        permit: CounterAddPermit,
    }

    impl ReadSendPermit {
        /// The max length of bytes authorized for send
        pub fn max_len(&self) -> usize {
            self.permit.len()
        }

        /// Send bytes to the front-end ReadStream.
        /// This function will panic if data.len() > max_len()
        pub fn send(self, data: bytes::Bytes) {
            let ReadSendPermit { sender, mut permit } = self;

            let data_len = data.len();
            let permit_len = permit.len();

            if data_len > permit_len {
                panic!("invalid data length");
            } else if data_len < permit_len {
                permit.downgrade_to(data_len);
            }

            sender.send((data, permit));
        }
    }

    /// The backend of a ReadStream, allows publishing data to the api user
    pub struct ReadStreamBackend {
        counter: Counter,
        send: MultiSenderPoll<(bytes::Bytes, CounterAddPermit)>,
        sender: Option<OnceSender<(bytes::Bytes, CounterAddPermit)>>,
        error_code: Arc<atomic::AtomicU64>,
        err: Arc<Mutex<Option<one_err::OneErr>>>,
    }

    impl ReadStreamBackend {
        /// Shutdown this read stream with given error
        pub fn stop(self, err: one_err::OneErr) {
            *self.err.lock() = Some(err);
        }

        /// Request to push data into the read stream.
        /// Err(u64) indicates the error code if the stream was stopped
        pub fn poll_acquire(
            &mut self,
            cx: &mut Context<'_>,
        ) -> Poll<Result<ReadSendPermit, u64>> {
            match self.poll_inner(cx) {
                Poll::Pending => Poll::Pending,
                Poll::Ready(Err(code)) => Poll::Ready(Err(code)),
                Poll::Ready(Ok((sender, permit))) => {
                    Poll::Ready(Ok(ReadSendPermit { sender, permit }))
                }
            }
        }

        /// Request to push data into the read stream.
        /// Err(u64) indicates the error code if the stream was stopped
        pub async fn acquire(&mut self) -> Result<ReadSendPermit, u64> {
            struct X<'lt>(&'lt mut ReadStreamBackend);

            impl<'lt> Future for X<'lt> {
                type Output = Result<
                    (
                        OnceSender<(bytes::Bytes, CounterAddPermit)>,
                        CounterAddPermit,
                    ),
                    u64,
                >;

                fn poll(
                    mut self: Pin<&mut Self>,
                    cx: &mut Context<'_>,
                ) -> Poll<Self::Output> {
                    self.0.poll_inner(cx)
                }
            }

            let (sender, permit) = X(self).await?;
            Ok(ReadSendPermit { sender, permit })
        }

        fn poll_inner(
            &mut self,
            cx: &mut Context<'_>,
        ) -> Poll<
            Result<
                (
                    OnceSender<(bytes::Bytes, CounterAddPermit)>,
                    CounterAddPermit,
                ),
                u64,
            >,
        > {
            let sender = if let Some(sender) = self.sender.take() {
                sender
            } else {
                match self.send.poll_acquire(cx) {
                    Poll::Pending => return Poll::Pending,
                    Poll::Ready(Err(_)) => {
                        let code =
                            self.error_code.load(atomic::Ordering::Acquire);
                        return Poll::Ready(Err(code));
                    }
                    Poll::Ready(Ok(sender)) => sender,
                }
            };

            if let Some(permit) = self.counter.reserve(cx) {
                Poll::Ready(Ok((sender, permit)))
            } else {
                self.sender = Some(sender);
                Poll::Pending
            }
        }
    }

    /// Construct a new read stream backend and frontend pair
    pub fn read_stream_pair<Runtime: AsyncRuntime>(
        buf_size: usize,
    ) -> (ReadStreamBackend, ReadStream) {
        let error_code = Arc::new(atomic::AtomicU64::new(0));
        let err = Arc::new(Mutex::new(None));
        let counter = Counter::new(buf_size);
        // if we're sending less than 8 byte chunks, we probably
        // deserve to be limited
        let chan_size = std::cmp::max(8, buf_size / 8);
        let (send, recv) = Runtime::channel(chan_size);
        (
            ReadStreamBackend {
                counter,
                send: MultiSenderPoll::new(send),
                sender: None,
                error_code: error_code.clone(),
                err: err.clone(),
            },
            ReadStream {
                recv,
                buf: None,
                error_code,
                err,
            },
        )
    }

    pub(crate) enum WriteCmdInner {
        Data(bytes::Bytes, CounterAddPermit),
        Stop(u64),
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
            if !b.is_empty() {
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
        /// Data sent over the write channel.
        /// This guard can dereference to a `&mut bytes::Bytes`.
        /// You call pull off as much or little data as you like
        /// when the guard is dropped, the sender side will be notified
        /// of any additional space available from bytes pulled off
        Data(WriteCmdData<'lt>),

        /// This write channel has been stopped with error_code,
        /// no more data will be forthcoming. If error_code is 0,
        /// it may indicate the fontend side was dropped
        Stop(u64),
    }

    /// The backend of a write stream, allows collecting the written data
    pub struct WriteStreamBackend {
        recv: MultiReceiver<WriteCmdInner>,
        buf: Option<WriteCmdInner>,
        error_code: Arc<atomic::AtomicU64>,
        err: Arc<Mutex<Option<one_err::OneErr>>>,
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
                        let code =
                            self.error_code.load(atomic::Ordering::Acquire);
                        return Poll::Ready(WriteCmdInner::Stop(code));
                    }
                    Poll::Ready(Some(cmd)) => cmd,
                }
            } {
                stop @ WriteCmdInner::Stop(_) => Poll::Ready(stop),
                WriteCmdInner::Data(b, _p) if b.is_empty() => {
                    self.poll_recv_inner(cx)
                }
                WriteCmdInner::Data(b, p) => {
                    Poll::Ready(WriteCmdInner::Data(b, p))
                }
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
                WriteCmdInner::Stop(error_code) => WriteCmd::Stop(error_code),
            }
        }

        /// Shut down this write stream with given err
        pub fn stop(self, err: one_err::OneErr) {
            *self.err.lock() = Some(err);
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
    pub fn write_stream_pair<Runtime: AsyncRuntime>(
        buf_size: usize,
    ) -> (WriteStreamBackend, WriteStream) {
        let counter = Counter::new(buf_size);
        // if we're sending less than 8 byte chunks, we probably
        // deserve to be limited
        let chan_size = std::cmp::max(8, buf_size / 8);
        let (send, recv) = Runtime::channel(chan_size);
        let error_code = Arc::new(atomic::AtomicU64::new(0));
        let err = Arc::new(Mutex::new(None));
        (
            WriteStreamBackend {
                recv,
                buf: None,
                error_code: error_code.clone(),
                err: err.clone(),
            },
            WriteStream {
                counter,
                send: MultiSenderPoll::new(send),
                sender: None,
                error_code,
                err,
            },
        )
    }
}

use backend::*;

/// Quic read stream
pub struct ReadStream {
    recv: MultiReceiver<(bytes::Bytes, CounterAddPermit)>,
    buf: Option<(bytes::Bytes, CounterAddPermit)>,
    error_code: Arc<atomic::AtomicU64>,
    err: Arc<Mutex<Option<one_err::OneErr>>>,
}

impl ReadStream {
    /// Read a chunk of data from the stream
    pub fn poll_read_bytes(
        &mut self,
        cx: &mut Context<'_>,
        max_len: usize,
    ) -> Poll<Option<AqResult<bytes::Bytes>>> {
        if self.buf.is_none() {
            match self.recv.poll_recv(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(None) => {
                    if let Some(err) = self.err.lock().take() {
                        return Poll::Ready(Some(Err(err)));
                    } else {
                        return Poll::Ready(None);
                    }
                }
                Poll::Ready(Some((b, p))) => self.buf = Some((b, p)),
            }
        }

        if max_len == 0 {
            return Poll::Ready(Some(Ok(bytes::Bytes::new())));
        }

        let (mut b, mut p) = self.buf.take().unwrap();

        if b.len() <= max_len {
            Poll::Ready(Some(Ok(b)))
        } else {
            let out = b.split_to(max_len);
            p.downgrade_to(max_len);
            self.buf = Some((b, p));
            Poll::Ready(Some(Ok(out)))
        }
    }

    /// Read a chunk of data from the stream
    pub async fn read_bytes(
        &mut self,
        max_len: usize,
    ) -> Option<AqResult<bytes::Bytes>> {
        struct X<'lt>(&'lt mut ReadStream, usize);

        impl<'lt> Future for X<'lt> {
            type Output = Option<AqResult<bytes::Bytes>>;

            fn poll(
                mut self: Pin<&mut Self>,
                cx: &mut Context<'_>,
            ) -> Poll<Self::Output> {
                let max_len = self.1;
                self.0.poll_read_bytes(cx, max_len)
            }
        }

        X(self, max_len).await
    }

    /// Read all data until this stream ends, up to max_len
    pub async fn read_to_end(
        mut self,
        mut max_len: usize,
    ) -> AqResult<buf_list::BufList<bytes::Bytes>> {
        let mut out = buf_list::BufList::new();
        while max_len > 0 {
            match self.read_bytes(max_len).await {
                None => break,
                Some(Err(e)) => return Err(e),
                Some(Ok(data)) => {
                    max_len -= data.len();
                    out.push_back(data);
                }
            }
        }
        Ok(out)
    }

    /// Cancel the stream with given error code
    pub fn stop(self, error_code: u64) {
        self.error_code.store(error_code, atomic::Ordering::Release);
    }
}

/// Quic write stream
pub struct WriteStream {
    counter: Counter,
    send: MultiSenderPoll<WriteCmdInner>,
    sender: Option<OnceSender<WriteCmdInner>>,
    error_code: Arc<atomic::AtomicU64>,
    err: Arc<Mutex<Option<one_err::OneErr>>>,
}

impl WriteStream {
    /// Write data to the stream
    pub fn poll_write_bytes(
        &mut self,
        cx: &mut Context<'_>,
        data: &mut bytes::Bytes,
    ) -> Poll<AqResult<()>> {
        let sender = if let Some(sender) = self.sender.take() {
            sender
        } else {
            match self.send.poll_acquire(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Err(e)) => {
                    if let Some(err) = self.err.lock().take() {
                        return Poll::Ready(Err(err));
                    } else {
                        return Poll::Ready(Err(e.into()));
                    }
                }
                Poll::Ready(Ok(sender)) => sender,
            }
        };

        match self.counter.reserve(cx) {
            None => {
                self.sender = Some(sender);
                Poll::Pending
            }
            Some(mut permit) => {
                let b = if data.len() >= permit.len() {
                    data.split_to(permit.len())
                } else {
                    let len = data.len();
                    permit.downgrade_to(len);
                    data.split_to(len)
                };
                sender.send(WriteCmdInner::Data(b, permit));
                Poll::Ready(Ok(()))
            }
        }
    }

    /// Write data to the stream
    pub async fn write_bytes(
        &mut self,
        data: &mut bytes::Bytes,
    ) -> AqResult<()> {
        struct X<'lt1, 'lt2>(&'lt1 mut WriteStream, &'lt2 mut bytes::Bytes);

        impl<'lt1, 'lt2> Future for X<'lt1, 'lt2> {
            type Output = AqResult<()>;

            fn poll(
                mut self: Pin<&mut Self>,
                cx: &mut Context<'_>,
            ) -> Poll<Self::Output> {
                let X(s, b) = &mut *self;
                s.poll_write_bytes(cx, b)
            }
        }

        X(self, data).await
    }

    /// Write data completely to the stream
    pub async fn write_bytes_all(
        &mut self,
        data: &mut bytes::Bytes,
    ) -> AqResult<()> {
        while !data.is_empty() {
            self.write_bytes(data).await?;
        }
        Ok(())
    }

    /// Stop this write channel with error_code
    pub fn stop(self, error_code: u64) {
        self.error_code.store(error_code, atomic::Ordering::Release);
    }
}

#[cfg(test)]
mod loom_tests;
