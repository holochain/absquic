//! Absquic_core utility types

use crate::sync::Arc;
use crate::*;
use std::future::Future;
use std::pin::Pin;
use std::task::Context;
use std::task::Poll;

/// One Shot Recv Future
pub type OneShotFut<T> = AqBoxFut<'static, AqResult<OneShotKind<T>>>;

/// One Shot Future Output Type
pub enum OneShotKind<T: 'static + Send> {
    /// A value was sent from the send side
    Value(T),

    /// A new future to poll was sent from the send side
    Forward(OneShotFut<T>),
}

/// Sender side of a one-shot channel
pub struct OneShotSender<T: 'static + Send> {
    #[allow(clippy::type_complexity)]
    send: Option<Box<dyn FnOnce(AqResult<OneShotKind<T>>) + 'static + Send>>,
}

impl<T: 'static + Send> Drop for OneShotSender<T> {
    fn drop(&mut self) {
        if let Some(send) = self.send.take() {
            send(Err("ChannelClosed".into()));
        }
    }
}

impl<T: 'static + Send> OneShotSender<T> {
    /// Construct a new one shot sender from a generic closure
    pub fn new<F>(f: F) -> Self
    where
        F: FnOnce(AqResult<OneShotKind<T>>) + 'static + Send,
    {
        Self {
            send: Some(Box::new(f)),
        }
    }

    /// Send the item to the receiver side of this channel
    pub fn send(mut self, r: AqResult<T>) {
        if let Some(send) = self.send.take() {
            match r {
                Err(e) => send(Err(e)),
                Ok(t) => send(Ok(OneShotKind::Value(t))),
            }
        }
    }

    /// Forward a different one shot receiver's result to
    /// the receiver attached to *this* one shot sender
    pub fn forward<F: Into<OneShotFut<T>>>(mut self, oth: F) {
        if let Some(send) = self.send.take() {
            send(Ok(OneShotKind::Forward(oth.into())));
        }
    }
}

/// Receiver side of a one-shot channel
#[must_use = "futures do nothing unless you `.await` or poll them"]
pub struct OneShotReceiver<T: 'static + Send> {
    recv: OneShotFut<T>,
}

impl<T: 'static + Send> From<OneShotReceiver<T>> for OneShotFut<T> {
    fn from(f: OneShotReceiver<T>) -> Self {
        f.recv
    }
}

impl<T: 'static + Send> From<OneShotFut<T>> for OneShotReceiver<T> {
    fn from(f: OneShotFut<T>) -> Self {
        OneShotReceiver { recv: f }
    }
}

impl<T: 'static + Send> Future for OneShotReceiver<T> {
    type Output = AqResult<T>;

    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Self::Output> {
        self.poll_recv(cx)
    }
}

impl<T: 'static + Send> OneShotReceiver<T> {
    /// Construct a new one shot receiver from a generic future
    pub fn new<F>(f: F) -> Self
    where
        F: Future<Output = AqResult<OneShotKind<T>>> + 'static + Send,
    {
        Self { recv: Box::pin(f) }
    }

    /// Extract the inner boxed future, useful for forwarding
    pub fn into_inner(self) -> OneShotFut<T> {
        self.recv
    }

    /// Forward the result of this receiver to a different one shot sender
    pub fn forward(self, oth: OneShotSender<T>) {
        oth.forward(self)
    }

    /// Future-aware poll receive function
    pub fn poll_recv(&mut self, cx: &mut Context<'_>) -> Poll<AqResult<T>> {
        match std::pin::Pin::new(&mut self.recv).poll(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Err(e)) => Poll::Ready(Err(e)),
            Poll::Ready(Ok(OneShotKind::Value(t))) => Poll::Ready(Ok(t)),
            Poll::Ready(Ok(OneShotKind::Forward(fut))) => {
                self.recv = fut;
                self.poll_recv(cx)
            }
        }
    }
}

/*
/// Backend type for the sender side of a channel
pub trait AsSend<T: 'static + Send>: 'static + Send + Sync {
    /// Is this channel still open?
    fn is_closed(&self) -> bool;

    /// Close this channel from the sender side
    fn close(&self);

    /// Attempt to acquire a send permit immediately
    fn try_acquire(&self) -> Option<OneShotSender<T>>;

    /// Get a future that will resolve into a send permit,
    /// or None if the channel was closed
    fn acquire(&self) -> AqBoxFut<'static, Option<OneShotSender<T>>>;
}

/// The sender side of a channel
pub struct Sender<T: 'static + Send> {
    send: Arc<dyn AsSend<T> + 'static + Send + Sync>,
    fut: Option<AqBoxFut<'static, Option<OneShotSender<T>>>>,
}

impl<T: 'static + Send> Clone for Sender<T> {
    fn clone(&self) -> Self {
        Self {
            send: self.send.clone(),
            fut: None,
        }
    }
}

impl<T: 'static + Send> Sender<T> {
    /// Construct a new sender instance
    pub fn new<S: AsSend<T>>(s: S) -> Self {
        let send: Arc<dyn AsSend<T> + 'static + Send + Sync> = Arc::new(s);
        Self { send, fut: None }
    }

    /// Is this channel still open?
    pub fn is_closed(&self) -> bool {
        self.send.is_closed()
    }

    /// Close this channel from the sender side
    pub fn close(&self) {
        self.send.close();
    }

    /// Attempt to acquire a send permit immediately
    pub fn try_acquire(&self) -> Option<OneShotSender<T>> {
        self.send.try_acquire()
    }

    /// Attempt to acquire a send permit
    pub fn poll_acquire(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Option<OneShotSender<T>>> {
        let mut fut = if let Some(fut) = self.fut.take() {
            fut
        } else {
            self.send.acquire()
        };

        match std::pin::Pin::new(&mut fut).poll(cx) {
            Poll::Pending => {
                self.fut = Some(fut);
                Poll::Pending
            }
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Ready(Some(sender)) => Poll::Ready(Some(sender)),
        }
    }

    /// Attempt to acquire a send permit
    pub async fn acquire(&mut self) -> Option<OneShotSender<T>> {
        struct X<'lt, T: 'static + Send>(&'lt mut Sender<T>);

        impl<'lt, T: 'static + Send> Future for X<'lt, T> {
            type Output = Option<OneShotSender<T>>;

            fn poll(
                mut self: Pin<&mut Self>,
                cx: &mut Context<'_>,
            ) -> Poll<Self::Output> {
                self.0.poll_acquire(cx)
            }
        }

        X(self).await
    }
}

/// Backend type for the receive side of a channel
pub trait AsReceive<T: 'static + Send>:
    'static + Send + futures_core::stream::Stream<Item = OneShotReceiver<T>>
{
}

/// The receiver side of a channel
pub struct Receiver<T: 'static + Send> {
    chan: std::pin::Pin<Box<dyn AsReceive<T>>>,
    item: Option<OneShotReceiver<T>>,
}

impl<T: 'static + Send> Receiver<T> {
    /// Construct a new receiver instance
    pub fn new<R: AsReceive<T>>(r: R) -> Self {
        let chan = Box::pin(r);
        Self { chan, item: None }
    }

    /// Receive data from this channel
    pub fn poll_recv(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<AqResult<T>> {
        let mut item = if let Some(item) = self.item.take() {
            item
        } else {
            match futures_core::stream::Stream::poll_next(
                self.chan.as_mut(),
                cx,
            ) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(None) => return Poll::Ready(Err("ChannelClosed".into())),
                Poll::Ready(Some(item)) => item,
            }
        };

        match item.poll_recv(cx) {
            Poll::Pending => {
                self.item = Some(item);
                Poll::Pending
            }
            Poll::Ready(Err(e)) => Poll::Ready(Err(e)),
            Poll::Ready(Ok(t)) => Poll::Ready(Ok(t)),
        }
    }

    /// Receive data from this channel
    pub async fn recv(&mut self) -> AqResult<T> {
        struct X<'lt, T: 'static + Send>(&'lt mut Receiver<T>);

        impl<'lt, T: 'static + Send> Future for X<'lt, T> {
            type Output = AqResult<T>;

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
*/

/// Absquic receive / stream type
pub trait Receive<T: 'static + Send>:
    'static + Send + futures_core::stream::Stream<Item = T>
{
}

/// Absquic receive / stream type
pub struct Receiver<T: 'static + Send> {
    recv: std::pin::Pin<Box<dyn AsReceive<T>>>,
}

impl<T: 'static + Send> Receiver<T> {
    /// Construct a new receiver instance
    pub fn new<R: Receive<T>>(r: R) -> Self {
        let recv = Box::pin(r);
        Self { recv }
    }

    /// Receive data from this channel
    pub fn poll_recv(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Option<T>> {
        futures_core::stream::Stream::poll_next(self.recv.as_mut(), cx)
    }

    /// Receive data from this channel
    pub async fn recv(&mut self) -> Option<T> {
        struct X<'lt, T: 'static + Send>(&'lt mut Receiver<T>);

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
