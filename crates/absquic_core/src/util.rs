//! absquic_core utility types

use crate::AqResult;
use parking_lot::Mutex;
use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::Context;
use std::task::Poll;
use std::task::Waker;

/// Util struct to aggregate wakers we want to wake later
/// usually after a synchronization lock has been released
pub struct WakeLater(Vec<Waker>);

impl Default for WakeLater {
    fn default() -> Self {
        WakeLater::new()
    }
}

impl WakeLater {
    /// construct a new WakeLater instance
    pub fn new() -> Self {
        Self(Vec::new())
    }

    /// push a waker into this instance
    #[inline(always)]
    pub fn push(&mut self, waker: Option<Waker>) {
        if let Some(waker) = waker {
            self.0.push(waker);
        }
    }

    /// wake all wakers aggregated by this instance
    pub fn wake(&mut self) {
        for waker in self.0.drain(..) {
            waker.wake();
        }
    }
}

struct OneShotInner<T: 'static + Send> {
    waker: Option<Waker>,
    value: Option<T>,
    closed: bool,
}

type OneShotCore<T> = Arc<Mutex<OneShotInner<T>>>;

/// Sender side of a one-shot channel
pub struct OneShotSender<T: 'static + Send>(OneShotCore<T>);

impl<T: 'static + Send> Drop for OneShotSender<T> {
    fn drop(&mut self) {
        if let Some(waker) = {
            let mut inner = self.0.lock();
            inner.closed = true;
            inner.waker.take()
        } {
            waker.wake();
        }
    }
}

impl<T: 'static + Send> OneShotSender<T> {
    /// send the item to the receiver side of this channel
    pub fn send(self, wake_later: &mut WakeLater, t: T) {
        let mut inner = self.0.lock();
        inner.closed = true;
        inner.value = Some(t);
        wake_later.push(inner.waker.take());
    }
}

/// Receiver side of a one-shot channel
pub struct OneShotReceiver<T: 'static + Send>(OneShotCore<T>);

impl<T: 'static + Send> OneShotReceiver<T> {
    /// future-aware poll receive function
    pub fn poll_recv(&mut self, cx: &mut Context<'_>) -> Poll<Option<T>> {
        let mut inner = self.0.lock();
        if let Some(t) = inner.value.take() {
            Poll::Ready(Some(t))
        } else if inner.closed {
            Poll::Ready(None)
        } else {
            inner.waker = Some(cx.waker().clone());
            Poll::Pending
        }
    }

    /// receive the item sent by the sender side of this channel
    pub async fn recv(mut self) -> Option<T> {
        struct X<'lt, T: 'static + Send>(&'lt mut OneShotReceiver<T>);

        impl<'lt, T: 'static + Send> Future for X<'lt, T> {
            type Output = Option<T>;

            fn poll(
                mut self: Pin<&mut Self>,
                cx: &mut Context<'_>,
            ) -> Poll<Self::Output> {
                self.0.poll_recv(cx)
            }
        }

        X(&mut self).await
    }
}

/// create a one-shot channel sender / receiver pair
pub fn one_shot_channel<T: 'static + Send>(
) -> (OneShotSender<T>, OneShotReceiver<T>) {
    let inner = OneShotInner {
        waker: None,
        value: None,
        closed: false,
    };
    let core = Arc::new(Mutex::new(inner));
    (OneShotSender(core.clone()), OneShotReceiver(core))
}

struct OutChanInner<T: 'static + Send> {
    closed: bool,
    read_waker: Option<Waker>,
    buffer: VecDeque<T>,
}

type OutChanCore<T> = Arc<Mutex<OutChanInner<T>>>;

/// sender side of an out-going data oriented channel
pub struct OutChanSender<T: 'static + Send>(OutChanCore<T>);

impl<T: 'static + Send> Drop for OutChanSender<T> {
    fn drop(&mut self) {
        if let Some(waker) = {
            let mut inner = self.0.lock();
            inner.closed = true;
            inner.read_waker.take()
        } {
            waker.wake();
        }
    }
}

impl<T: 'static + Send> OutChanSender<T> {
    /// push data into this channel
    pub fn push(
        &self,
        should_close: &mut bool,
        wake_later: &mut WakeLater,
        data: &mut VecDeque<T>,
        close: bool,
    ) {
        let mut inner = self.0.lock();
        inner.buffer.append(data);
        wake_later.push(inner.read_waker.take());
        if close {
            inner.closed = true;
        }
        if inner.closed {
            *should_close = true;
        }
    }
}

/// receiver side of an out-going data oriented channel
pub struct OutChanReceiver<T: 'static + Send>(OutChanCore<T>);

impl<T: 'static + Send> Drop for OutChanReceiver<T> {
    fn drop(&mut self) {
        self.0.lock().closed = true;
    }
}

impl<T: 'static + Send> OutChanReceiver<T> {
    /// Receive data sent by the sender on this channel
    pub fn poll_recv(&mut self, cx: &mut Context<'_>) -> Poll<Option<T>> {
        let mut inner = self.0.lock();
        if inner.buffer.is_empty() {
            if inner.closed {
                Poll::Ready(None)
            } else {
                inner.read_waker = Some(cx.waker().clone());
                Poll::Pending
            }
        } else {
            Poll::Ready(inner.buffer.pop_front())
        }
    }

    /// receive data sent by the sender on this channel
    pub async fn recv(&mut self) -> Option<T> {
        struct X<'lt, T: 'static + Send>(&'lt mut OutChanReceiver<T>);

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

/// create a new out-going data oriented channel pair
pub fn out_chan<T: 'static + Send>() -> (OutChanSender<T>, OutChanReceiver<T>) {
    let inner = OutChanInner {
        closed: false,
        read_waker: None,
        buffer: VecDeque::new(),
    };
    let core = Arc::new(Mutex::new(inner));
    (OutChanSender(core.clone()), OutChanReceiver(core))
}

struct InChanInner<T: 'static + Send> {
    closed: bool,
    buffer: VecDeque<T>,
}

type InChanCore<T> = Arc<Mutex<InChanInner<T>>>;

/// sender side of an incoming data channel
pub struct InChanSender<T: 'static + Send>(InChanCore<T>);

impl<T: 'static + Send> Clone for InChanSender<T> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<T: 'static + Send> Drop for InChanSender<T> {
    fn drop(&mut self) {
        self.0.lock().closed = true;
    }
}

impl<T: 'static + Send> InChanSender<T> {
    /// send data into the channel
    pub fn send(&self, t: T) -> AqResult<()> {
        let mut inner = self.0.lock();
        if inner.closed {
            Err("ChannelClosed".into())
        } else {
            inner.buffer.push_back(t);
            Ok(())
        }
    }
}

/// receiver side of an incoming data channel
pub struct InChanReceiver<T: 'static + Send>(InChanCore<T>);

impl<T: 'static + Send> Drop for InChanReceiver<T> {
    fn drop(&mut self) {
        self.0.lock().closed = true;
    }
}

impl<T: 'static + Send> InChanReceiver<T> {
    /// receive data from the channel
    pub fn recv(
        &self,
        should_close: &mut bool,
        dest: &mut VecDeque<T>,
        close: bool,
    ) {
        let mut inner = self.0.lock();
        dest.append(&mut inner.buffer);
        if close {
            inner.closed = true;
        }
        if inner.closed {
            *should_close = true;
        }
    }
}

/// create a new incoming data oriented channel pair
pub fn in_chan<T: 'static + Send>() -> (InChanSender<T>, InChanReceiver<T>) {
    let inner = InChanInner {
        closed: false,
        buffer: VecDeque::new(),
    };
    let core = Arc::new(Mutex::new(inner));
    (InChanSender(core.clone()), InChanReceiver(core))
}
