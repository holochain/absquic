/// TimeoutsScheduler powered by tokio time feature
pub struct TokioTimeoutsScheduler(Option<tokio::task::JoinHandle<()>>);

impl Default for TokioTimeoutsScheduler {
    fn default() -> Self {
        TokioTimeoutsScheduler::new()
    }
}

impl TokioTimeoutsScheduler {
    /// Construct a new TimeoutScheduler powered by tokio time feature
    pub fn new() -> Self {
        Self(None)
    }
}

impl absquic_core::endpoint::TimeoutsScheduler for TokioTimeoutsScheduler {
    fn schedule(
        &mut self,
        logic: Box<dyn FnOnce() + 'static + Send>,
        at: std::time::Instant,
    ) {
        if let Some(t) = self.0.take() {
            t.abort();
        }
        self.0 = Some(tokio::task::spawn(async move {
            tokio::time::sleep_until(at.into()).await;
            logic();
        }));
    }
}
