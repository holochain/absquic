use absquic_core::rt::*;
use absquic_core::*;
use std::sync::Arc;

pub(crate) type Guard<R> = <<R as Rt>::Semaphore as Semaphore>::GuardTy;
pub(crate) type Limit<R> = <R as Rt>::Semaphore;

struct BoundMultiSendInner<R: Rt, T: 'static + Send> {
    limit: Limit<R>,
    send: DynMultiSend<(T, Guard<R>)>,
}

pub(crate) struct BoundMultiSend<R: Rt, T: 'static + Send>(
    Arc<BoundMultiSendInner<R, T>>,
);

impl<R: Rt, T: 'static + Send> Clone for BoundMultiSend<R, T> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<R: Rt, T: 'static + Send> BoundMultiSend<R, T> {
    pub fn try_acquire(&self) -> Option<Guard<R>> {
        self.0.limit.try_acquire()
    }

    pub async fn acquire(&self) -> Guard<R> {
        self.0.limit.acquire().await
    }

    pub fn send(&self, t: T, g: Guard<R>) -> Result<()> {
        self.0.send.send((t, g))
    }
}

pub(crate) fn bound_channel<R: Rt, T: 'static + Send>(
    limit: usize,
) -> (BoundMultiSend<R, T>, BoxRecv<'static, (T, Guard<R>)>) {
    let limit = R::semaphore(limit);
    let (send, recv) = R::channel();
    (
        BoundMultiSend(Arc::new(BoundMultiSendInner { limit, send })),
        recv,
    )
}
