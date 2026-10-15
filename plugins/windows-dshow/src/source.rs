//! Reads the app's frames from the shared `OVC1` ring, reconnecting as the app starts,
//! stops or restarts.

use std::time::{Duration, Instant};

use ovc_ipc::{Consumer, Region, VCAM_RING_NAME};

/// How long the last frame keeps being shown after the app stops publishing.
const STALE_AFTER: Duration = Duration::from_secs(2);
/// How often to retry opening the ring while the app is not running.
const RETRY_EVERY: Duration = Duration::from_millis(500);

pub struct RingSource {
    consumer: Option<Consumer>,
    buf: Vec<u8>,
    w: u32,
    h: u32,
    last_frame: Option<Instant>,
    last_try: Option<Instant>,
}

impl RingSource {
    pub fn new() -> Self {
        Self { consumer: None, buf: Vec::new(), w: 0, h: 0, last_frame: None, last_try: None }
    }

    /// Tell the Studio an app is pulling `w`x`h` frames right now. While no app is, the
    /// Studio stops publishing (saves a full-frame copy per frame).
    pub fn report(&self, w: u32, h: u32) {
        if let Some(c) = &self.consumer {
            c.report(w, h);
        }
    }

    /// Pull a newer frame from the ring if there is one. Returns `true` if one arrived.
    pub fn refresh(&mut self) -> bool {
        let now = Instant::now();
        if self.consumer.is_none() && self.last_try.map_or(true, |t| now - t >= RETRY_EVERY) {
            self.last_try = Some(now);
            self.consumer = Region::shared(VCAM_RING_NAME, 0, false).and_then(Consumer::open).ok();
        }
        let mut fresh = false;
        if let Some(c) = self.consumer.as_mut() {
            if c.try_recv(&mut self.buf).is_some() {
                let (w, h, _) = c.format();
                if w > 0 && h > 0 && self.buf.len() == w as usize * h as usize * 4 {
                    self.w = w;
                    self.h = h;
                    self.last_frame = Some(now);
                    fresh = true;
                }
            }
            if !c.producer_alive() {
                // App closed: release the mapping; a restarted app creates a new one we reopen.
                self.consumer = None;
                self.last_frame = None;
            }
        }
        if self.last_frame.is_some_and(|t| now - t >= STALE_AFTER) {
            self.last_frame = None; // app alive but not publishing (camera stalled / vcam off)
        }
        fresh
    }

    /// The newest RGBA frame `(data, w, h)`, or `None` if the app isn't publishing.
    pub fn frame(&self) -> Option<(&[u8], u32, u32)> {
        self.last_frame.map(|_| (&self.buf[..], self.w, self.h))
    }
}
