use parking_lot::Mutex;
use std::collections::VecDeque;
use std::sync::Arc;

struct OutChanInner<T> {
    new_data_waker: Option<std::task::Waker>,
    data: VecDeque<T>,
}

impl<T> OutChanInner<T> {
    fn new(capacity: usize) -> Self {
        Self {
            new_data_waker: None,
            data: VecDeque::with_capacity(capacity),
        }
    }
}

pub struct OutChan<T>(Arc<Mutex<OutChanInner<T>>>);

impl<T> Clone for OutChan<T> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<T> OutChan<T> {
    pub fn new(capacity: usize) -> Self {
        Self(Arc::new(Mutex::new(OutChanInner::new(capacity))))
    }

    pub fn push_back(&self, t: &mut VecDeque<T>) {
        let waker = {
            let mut inner = self.0.lock();
            inner.data.append(t);
            inner.new_data_waker.take()
        };
        if let Some(waker) = waker {
            waker.wake();
        }
    }

    pub fn poll_recv(
        &self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<T>> {
        let mut inner = self.0.lock();
        if inner.data.is_empty() {
            inner.new_data_waker = Some(cx.waker().clone());
            std::task::Poll::Pending
        } else {
            std::task::Poll::Ready(inner.data.pop_front())
        }
    }
}
