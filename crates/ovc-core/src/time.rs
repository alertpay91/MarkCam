//! Presentation clock and timestamps (nanoseconds, monotonic).

use std::time::Instant;

/// A presentation timestamp in nanoseconds since the pipeline clock started.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Timestamp(pub i64);

impl Timestamp {
    pub const ZERO: Timestamp = Timestamp(0);
    pub fn as_nanos(self) -> i64 {
        self.0
    }
}

/// Monotonic clock driving the pipeline cadence.
pub struct Clock {
    start: Instant,
}

impl Clock {
    pub fn new() -> Self {
        Self { start: Instant::now() }
    }
    pub fn now(&self) -> Timestamp {
        Timestamp(self.start.elapsed().as_nanos() as i64)
    }
}

impl Default for Clock {
    fn default() -> Self {
        Self::new()
    }
}
