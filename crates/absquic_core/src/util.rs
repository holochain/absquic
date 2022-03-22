//! absquic_core utility types

use crate::*;
use parking_lot::Mutex;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
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
    /// construct a new one shot sender from a generic closure
    pub fn new<F>(f: F) -> Self
    where
        F: FnOnce(AqResult<OneShotKind<T>>) + 'static + Send,
    {
        Self {
            send: Some(Box::new(f)),
        }
    }

    /// send the item to the receiver side of this channel
    pub fn send(mut self, r: AqResult<T>) {
        if let Some(send) = self.send.take() {
            match r {
                Err(e) => send(Err(e)),
                Ok(t) => send(Ok(OneShotKind::Value(t))),
            }
        }
    }

    /// forward a different one shot receiver's result to
    /// the receiver attached to *this* one shot sender.
    pub fn forward(mut self, oth: OneShotReceiver<T>) {
        if let Some(send) = self.send.take() {
            send(Ok(OneShotKind::Forward(oth.recv)));
        }
    }
}

/// Receiver side of a one-shot channel
#[must_use = "futures do nothing unless you `.await` or poll them"]
pub struct OneShotReceiver<T: 'static + Send> {
    recv: OneShotFut<T>,
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
    /// construct a new one shot receiver from a generic future
    pub fn new<F>(f: F) -> Self
    where
        F: Future<Output = AqResult<OneShotKind<T>>> + 'static + Send,
    {
        Self { recv: Box::pin(f) }
    }

    /// forward the result of this receiver to a different one shot sender
    pub fn forward(self, oth: OneShotSender<T>) {
        oth.forward(self)
    }

    /// future-aware poll receive function
    pub fn poll_recv(&mut self, cx: &mut Context<'_>) -> Poll<AqResult<T>> {
        match std::pin::Pin::new(&mut self.recv).poll(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Err(e)) => Poll::Ready(Err(e)),
            Poll::Ready(Ok(OneShotKind::Value(t))) => Poll::Ready(Ok(t)),
            Poll::Ready(Ok(OneShotKind::Forward(fut))) => {
                self.recv = fut;
                // this should tail recurse, right?
                self.poll_recv(cx)
            }
        }
    }
}

/// create a one-shot channel sender / receiver pair
pub fn one_shot_channel<T: 'static + Send>(
) -> (OneShotSender<T>, OneShotReceiver<T>) {
    let (s, r) = tokio::sync::oneshot::channel::<AqResult<OneShotKind<T>>>();
    (
        OneShotSender::new(move |t| {
            let _ = s.send(t);
        }),
        OneShotReceiver::new(async move {
            match r.await {
                Err(_) => Err("ChannelClosed".into()),
                Ok(k) => k,
            }
        }),
    )
}

/// sender side of a data channel
pub struct Sender<T: 'static + Send> {
    sender: Option<Arc<Mutex<Option<tokio::sync::mpsc::Sender<T>>>>>,
    fut: Option<AqBoxFut<'static, AqResult<tokio::sync::mpsc::OwnedPermit<T>>>>,
}

/// sender send callback type
pub type SenderCb<T> = Box<dyn FnOnce(T) + 'static + Send>;

impl<T: 'static + Send> Clone for Sender<T> {
    fn clone(&self) -> Self {
        Sender {
            sender: self.sender.clone(),
            // clone will not have the same place in the send queue
            fut: None,
        }
    }
}

impl<T: 'static + Send> Sender<T> {
    fn get_sender(&self) -> AqResult<tokio::sync::mpsc::Sender<T>> {
        if let Some(inner) = &self.sender {
            let inner = inner.lock();
            if let Some(inner) = &*inner {
                return Ok(inner.clone());
            }
        }
        Err("ChannelClosed".into())
    }

    /// close this channel from the sender side
    pub fn close(&mut self) {
        drop(self.fut.take());
        if let Some(inner) = &self.sender {
            drop(inner.lock().take());
        }
        drop(self.sender.take());
    }

    /// check if this channel is closed
    pub fn is_closed(&self) -> bool {
        if let Some(inner) = &self.sender {
            let inner = inner.lock();
            if let Some(inner) = &*inner {
                return inner.is_closed();
            }
        }
        true
    }

    /// attempt to send data on this channel
    pub fn poll_send(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<AqResult<SenderCb<T>>> {
        let mut fut = if let Some(fut) = self.fut.take() {
            fut
        } else {
            let sender = match self.get_sender() {
                Err(e) => return Poll::Ready(Err(e)),
                Ok(sender) => sender,
            };

            let fut = sender.reserve_owned();
            Box::pin(async move {
                fut.await.map_err(|_| one_err::OneErr::new("ChannelClosed"))
            })
        };

        match std::pin::Pin::new(&mut fut).poll(cx) {
            Poll::Pending => {
                self.fut = Some(fut);
                return Poll::Pending;
            }
            Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
            Poll::Ready(Ok(permit)) => {
                let cb: SenderCb<T> = Box::new(move |t| {
                    let _ = permit.send(t);
                });
                return Poll::Ready(Ok(cb));
            }
        }
    }

    /// attempt to send data on this channel
    pub async fn send(&mut self, t: T) -> AqResult<()> {
        struct X<'lt, T: 'static + Send>(&'lt mut Sender<T>);

        impl<'lt, T: 'static + Send> Future for X<'lt, T> {
            type Output = AqResult<SenderCb<T>>;

            fn poll(
                mut self: Pin<&mut Self>,
                cx: &mut Context<'_>,
            ) -> Poll<Self::Output> {
                self.0.poll_send(cx)
            }
        }

        let cb = X(self).await?;
        cb(t);
        Ok(())
    }
}

/// receiver side of a data channel
pub struct Receiver<T: 'static + Send> {
    receiver: Option<tokio::sync::mpsc::Receiver<T>>,
}

impl<T: 'static + Send> Receiver<T> {
    /// receive data from this channel
    pub fn poll_recv(&mut self, cx: &mut Context<'_>) -> Poll<Option<T>> {
        if let Some(r) = &mut self.receiver {
            r.poll_recv(cx)
        } else {
            Poll::Ready(None)
        }
    }

    /// receive data from this channel
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

/// create a new data channel sender receiver pair
pub fn channel<T: 'static + Send>(bound: usize) -> (Sender<T>, Receiver<T>) {
    let (s, r) = tokio::sync::mpsc::channel(bound);
    let s = Sender {
        sender: Some(Arc::new(Mutex::new(Some(s)))),
        fut: None,
    };
    let r = Receiver { receiver: Some(r) };
    (s, r)
}
