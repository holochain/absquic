//! Absquic_core runtime types

use crate::*;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::Context;
use std::task::Poll;

/// ChannelClosed
#[derive(Debug, Clone, Copy)]
pub struct ChannelClosed;

impl std::fmt::Display for ChannelClosed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ChannelClosed")
    }
}

impl std::error::Error for ChannelClosed {}

impl From<ChannelClosed> for one_err::OneErr {
    fn from(_: ChannelClosed) -> Self {
        one_err::OneErr::new("ChannelClosed")
    }
}

/// Channel result
pub type ChanResult<T> = std::result::Result<T, ChannelClosed>;

/// Abstraction for a particular async / futures runtime
pub trait AsyncRuntime: 'static + Send + Sync {
    /// Spawn a task to execute the given future, return a future
    /// that will complete when the task completes, but will not
    /// affect the task if dropped / cancelled
    fn spawn<R, F>(f: F) -> SpawnHnd<R>
    where
        R: 'static + Send,
        F: Future<Output = AqResult<R>> + 'static + Send;

    /// Returns a future that will complete at the specified instant,
    /// or immediately if the instant is already past
    fn sleep(time: std::time::Instant) -> AqFut<'static, ()>;

    /// Create an async semaphore instance
    fn semaphore(limit: usize) -> DynSemaphore;

    /// Creates a slot for a single data transfer
    fn one_shot<T: 'static + Send>(
    ) -> (OnceSender<T>, AqFut<'static, Option<T>>);

    /// Create a multiple-producer single-consumer channel send/recv pair
    fn channel<T: 'static + Send>(
        bound: usize,
    ) -> (MultiSender<T>, MultiReceiver<T>);
}

/// A completion handle to a spawned task.
/// The task will continue even if this handle is dropped.
/// Note, this struct implements Future but is intentionally NOT #\[must_use\]
pub struct SpawnHnd<R: 'static + Send>(AqFut<'static, AqResult<R>>);

impl<R: 'static + Send> SpawnHnd<R> {
    /// Construct a new SpawnHnd from a generic future
    #[inline(always)]
    pub fn new<F>(f: F) -> Self
    where
        F: Future<Output = AqResult<R>> + 'static + Send,
    {
        Self(AqFut::new(f))
    }
}

impl<R: 'static + Send> Future for SpawnHnd<R> {
    type Output = AqResult<R>;

    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Self::Output> {
        std::pin::Pin::new(&mut self.0).poll(cx)
    }
}

/// Abstract async semaphore guard implementation
pub trait SemaphoreGuard: 'static + Send {}

/// Abstract async semaphore guard implementation
pub type DynSemaphoreGuard = Box<dyn SemaphoreGuard + 'static + Send>;

/// Abstract async semaphore implementation
pub trait Semaphore: 'static + Send + Sync {
    /// Acquire a semaphore guard
    fn acquire(&self) -> AqFut<'static, DynSemaphoreGuard>;
}

/// Abstract async semaphore implementation
pub type DynSemaphore = Arc<dyn Semaphore + 'static + Send + Sync>;

/// Sender type for a one_shot slot
pub struct OnceSender<T: 'static + Send>(Box<dyn FnOnce(T) + 'static + Send>);

impl<T: 'static + Send> OnceSender<T> {
    /// Construct the sender side of a one_shot slot
    #[inline(always)]
    pub fn new<S: FnOnce(T) + 'static + Send>(s: S) -> Self {
        Self(Box::new(s))
    }

    /// Send data to the receive side of this one_shot slot
    #[inline(always)]
    pub fn send(self, t: T) {
        (self.0)(t);
    }
}

/// Sender type for a bound mpsc channel
pub trait MultiSend<T: 'static + Send>: 'static + Send + Sync {
    /// Attempt to acquire a permit to send data into the channel.
    /// The returned future is cancel safe in the sense that the queue
    /// position awaiting a send permit will be released
    fn acquire(&self) -> AqFut<'static, ChanResult<OnceSender<T>>>;
}

/// Wrapper sender type for a bound mpsc channel that allows polling
/// and in return is `!Sync`
pub struct MultiSenderPoll<T: 'static + Send> {
    send: MultiSender<T>,
    fut: Option<AqFut<'static, ChanResult<OnceSender<T>>>>,
}

impl<T: 'static + Send> MultiSenderPoll<T> {
    /// Construct a new multi sender poll instance
    #[inline(always)]
    pub fn new(sender: MultiSender<T>) -> Self {
        Self {
            send: sender,
            fut: None,
        }
    }

    /// Get a reference to the inner sender
    pub fn as_inner(&self) -> &MultiSender<T> {
        &self.send
    }

    /// Extract the inner sender
    pub fn into_inner(self) -> MultiSender<T> {
        self.send
    }

    /// Attempt to acquire a permit to send data into the channel.
    pub fn poll_acquire(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<ChanResult<OnceSender<T>>> {
        let mut fut = if let Some(fut) = self.fut.take() {
            fut
        } else {
            self.send.acquire()
        };

        match Pin::new(&mut fut).poll(cx) {
            Poll::Pending => {
                self.fut = Some(fut);
                Poll::Pending
            }
            Poll::Ready(Err(e)) => Poll::Ready(Err(e)),
            Poll::Ready(Ok(sender)) => Poll::Ready(Ok(sender)),
        }
    }

    /// Attempt to acquire a permit to send data into the channel.
    /// The returned future is cancel safe in the sense that the queue
    /// position awaiting a send permit will be released
    #[inline(always)]
    pub fn acquire(&self) -> AqFut<'static, ChanResult<OnceSender<T>>> {
        self.send.acquire()
    }
}

/// Sender type for a bound mpsc channel
pub struct MultiSender<T: 'static + Send> {
    send: Arc<dyn MultiSend<T> + 'static + Send + Sync>,
}

impl<T: 'static + Send> Clone for MultiSender<T> {
    fn clone(&self) -> Self {
        MultiSender {
            send: self.send.clone(),
        }
    }
}

impl<T: 'static + Send> MultiSender<T> {
    /// Construct a new multi sender instance
    #[inline(always)]
    pub fn new<S: MultiSend<T> + 'static + Send + Sync>(s: S) -> Self {
        Self { send: Arc::new(s) }
    }

    /// Attempt to acquire a permit to send data into the channel.
    /// The returned future is cancel safe in the sense that the queue
    /// position awaiting a send permit will be released
    #[inline(always)]
    pub fn acquire(&self) -> AqFut<'static, ChanResult<OnceSender<T>>> {
        self.send.acquire()
    }
}

/// Receiver type for a bound mpsc channel
pub struct MultiReceiver<T: 'static + Send>(
    Pin<Box<dyn futures_core::stream::Stream<Item = T> + 'static + Send>>,
);

impl<T: 'static + Send> MultiReceiver<T> {
    /// Construct a new multi receiver instance
    #[inline(always)]
    pub fn new<R>(r: R) -> Self
    where
        R: futures_core::stream::Stream<Item = T> + 'static + Send,
    {
        Self(Box::pin(r))
    }

    /// Receive data from this channel
    pub fn poll_recv(&mut self, cx: &mut Context<'_>) -> Poll<Option<T>> {
        futures_core::stream::Stream::poll_next(self.0.as_mut(), cx)
    }

    /// Receive data from this channel
    pub async fn recv(&mut self) -> Option<T> {
        struct X<'lt, T: 'static + Send>(&'lt mut MultiReceiver<T>);

        impl<'lt, T: 'static + Send> Future for X<'lt, T> {
            type Output = Option<T>;

            fn poll(
                mut self: Pin<&mut Self>,
                cx: &mut Context<'_>,
            ) -> Poll<Self::Output> {
                self.0.poll_recv(cx)
            }
        }

        X(self).await
    }
}

impl<T: 'static + Send> futures_core::stream::Stream for MultiReceiver<T> {
    type Item = T;

    fn poll_next(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Self::Item>> {
        self.poll_recv(cx)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.0.size_hint()
    }
}
