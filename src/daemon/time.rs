use std::time::{SystemTime, UNIX_EPOCH};

pub fn seconds_since_epoch() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("Time must be > 1970")
        .as_secs()
}
