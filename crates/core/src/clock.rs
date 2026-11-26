//! Injected time. Engine code never reads the wall clock directly: it is handed a [`Clock`], so
//! a decision can be replayed with the timestamps it originally saw.

use std::sync::atomic::{AtomicU64, Ordering};

use nautilus_core::UnixNanos;

/// A source of the current time.
pub trait Clock: Send + Sync {
    fn now(&self) -> UnixNanos;
}

/// The wall clock. Only for process edges (binaries, the broker adapter); never construct one
/// inside strategy, risk or decision logic.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> UnixNanos {
        let since_epoch = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        UnixNanos::from(u64::try_from(since_epoch.as_nanos()).unwrap_or(u64::MAX))
    }
}

/// A clock that only moves when told to: for backtests, replay and tests.
#[derive(Debug, Default)]
pub struct ManualClock(AtomicU64);

impl ManualClock {
    #[must_use]
    pub fn new(now: UnixNanos) -> Self {
        Self(AtomicU64::new(now.as_u64()))
    }

    pub fn set(&self, now: UnixNanos) {
        self.0.store(now.as_u64(), Ordering::SeqCst);
    }

    pub fn advance_nanos(&self, nanos: u64) {
        self.0.fetch_add(nanos, Ordering::SeqCst);
    }
}

impl Clock for ManualClock {
    fn now(&self) -> UnixNanos {
        UnixNanos::from(self.0.load(Ordering::SeqCst))
    }
}

impl<C: Clock + ?Sized> Clock for &C {
    fn now(&self) -> UnixNanos {
        (**self).now()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manual_clock_moves_only_when_told() {
        let clock = ManualClock::new(UnixNanos::from(10));
        assert_eq!(clock.now(), UnixNanos::from(10));
        assert_eq!(clock.now(), UnixNanos::from(10));
        clock.advance_nanos(5);
        assert_eq!(clock.now(), UnixNanos::from(15));
        clock.set(UnixNanos::from(3));
        fn read(clock: impl Clock) -> UnixNanos {
            clock.now()
        }
        assert_eq!(read(&clock), UnixNanos::from(3));
    }

    #[test]
    fn system_clock_is_after_2026() {
        assert!(SystemClock.now().as_u64() > 1_767_225_600_000_000_000);
    }
}
