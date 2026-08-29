use std::sync::{Mutex, MutexGuard, OnceLock};

/// Serialize tests that mutate process-global environment variables.
///
/// Rust's test harness runs modules concurrently inside one process. Every test
/// that calls `std::env::set_var` or `remove_var` must hold this crate-wide
/// guard for the complete mutation/restore window.
pub(crate) fn lock() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}
