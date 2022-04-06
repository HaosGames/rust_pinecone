use std::time::{Duration, SystemTime};

#[derive(Debug)]
pub(crate) struct WaitTimer {
    last_time: SystemTime,
    duration: Duration,
}
impl WaitTimer {
    pub(crate) fn new(duration: Duration) -> Self {
        Self {
            last_time: SystemTime::now(),
            duration,
        }
    }
    pub(crate) fn is_expired(&self) -> bool {
        match SystemTime::now().duration_since(self.last_time) {
            Ok(duration) => {
                if duration > self.duration {
                    true
                } else {
                    false
                }
            }
            Err(_) => false,
        }
    }
    pub(crate) fn new_expired() -> WaitTimer {
        WaitTimer {
            last_time: SystemTime::now(),
            duration: Duration::ZERO,
        }
    }
}
