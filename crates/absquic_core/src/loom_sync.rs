pub use loom::future::block_on;
pub use loom::sync::atomic;
pub use loom::sync::Arc;
pub use loom::thread;

pub struct Mutex<T>(loom::sync::Mutex<T>);
impl<T> Mutex<T> {
    pub fn new(data: T) -> Self {
        Self(loom::sync::Mutex::new(data))
    }
    pub fn lock(&self) -> loom::sync::MutexGuard<'_, T> {
        self.0.lock().unwrap()
    }
}
