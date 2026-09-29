//! Notices an engine that started fine but never gets audio from the mic.
//!
//! macOS can report microphone access as granted while withholding capture
//! from this process (the permission prompt for the terminal is still pending,
//! or the terminal/IDE isn't the app macOS attributes the request to). The
//! engine then runs without errors and without a single input callback, which
//! looks like "running" with silent meters. Shared by the TUI and `presence run`.

use std::time::{Duration, Instant};

/// How long the input callback count may stand still before it's a stall.
pub const STALL_AFTER: Duration = Duration::from_secs(3);

/// What an [`Watchdog::update`] means for the warning.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Change {
    /// No callbacks for [`STALL_AFTER`]: start showing the warning.
    Stalled,
    /// Callbacks resumed after a stall: clear it.
    Recovered,
    None,
}

#[derive(Clone, Debug)]
pub struct Watchdog {
    count: u64,
    progress_at: Instant,
    stalled: bool,
}

impl Watchdog {
    /// Start watching an engine whose callback count is `count` at `now`.
    pub fn new(now: Instant, count: u64) -> Self {
        Self {
            count,
            progress_at: now,
            stalled: false,
        }
    }

    /// Feed the engine's current callback count; reports stalls and
    /// recoveries once each, when they happen.
    pub fn update(&mut self, now: Instant, count: u64) -> Change {
        if count != self.count {
            self.count = count;
            self.progress_at = now;
            if std::mem::take(&mut self.stalled) {
                return Change::Recovered;
            }
        } else if !self.stalled && now.saturating_duration_since(self.progress_at) >= STALL_AFTER {
            self.stalled = true;
            return Change::Stalled;
        }
        Change::None
    }

    /// Whether the warning should be showing.
    pub fn stalled(&self) -> bool {
        self.stalled
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: Duration = Duration::from_millis(1);

    #[test]
    fn no_callbacks_since_start_is_a_stall() {
        let t0 = Instant::now();
        let mut w = Watchdog::new(t0, 0);
        assert_eq!(w.update(t0 + STALL_AFTER - MS, 0), Change::None);
        assert!(!w.stalled());
        assert_eq!(w.update(t0 + STALL_AFTER, 0), Change::Stalled);
        assert!(w.stalled());
        // Reported once, and stays on.
        assert_eq!(w.update(t0 + STALL_AFTER * 5, 0), Change::None);
        assert!(w.stalled());
    }

    #[test]
    fn flowing_audio_never_stalls() {
        let t0 = Instant::now();
        let mut w = Watchdog::new(t0, 0);
        for i in 1..100u32 {
            assert_eq!(
                w.update(t0 + STALL_AFTER / 2 * i, u64::from(i)),
                Change::None
            );
        }
        assert!(!w.stalled());
    }

    #[test]
    fn stall_is_timed_from_the_last_increase() {
        let t0 = Instant::now();
        let mut w = Watchdog::new(t0, 7);
        let t1 = t0 + Duration::from_secs(2);
        assert_eq!(w.update(t1, 8), Change::None);
        assert_eq!(w.update(t0 + STALL_AFTER, 8), Change::None);
        assert_eq!(w.update(t1 + STALL_AFTER - MS, 8), Change::None);
        assert_eq!(w.update(t1 + STALL_AFTER, 8), Change::Stalled);
    }

    #[test]
    fn recovers_when_callbacks_start_and_can_stall_again() {
        let t0 = Instant::now();
        let mut w = Watchdog::new(t0, 0);
        assert_eq!(w.update(t0 + STALL_AFTER, 0), Change::Stalled);
        let t1 = t0 + Duration::from_secs(10);
        assert_eq!(w.update(t1, 1), Change::Recovered);
        assert!(!w.stalled());
        assert_eq!(w.update(t1 + MS, 2), Change::None);
        assert_eq!(w.update(t1 + MS + STALL_AFTER, 2), Change::Stalled);
        assert!(w.stalled());
    }
}
