#[path = "../../common/bitcoind.rs"]
pub mod bitcoind;

use std::sync::{Mutex, OnceLock};

pub fn test_guard() -> std::sync::MutexGuard<'static, ()> {
    static TEST_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    TEST_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .expect("test lock poisoned")
}
