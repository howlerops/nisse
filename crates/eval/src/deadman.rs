//! The dead-man check: is the newest heartbeat of a source young enough?
//!
//! The engine writes `heartbeat` rows periodically; this judges the newest one against
//! `heartbeat_max_age_seconds` from the limits file. Silence and a clock running ahead both
//! fail it. The binary `nisse-deadman` wraps this for cron.

use nisse_core::nautilus::UnixNanos;
use nisse_store::tables::heartbeat;
use tokio_postgres::GenericClient;

use crate::EvalError;

const NANOS_PER_SECOND: u64 = 1_000_000_000;

/// What the dead-man check found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Liveness {
    /// The newest heartbeat is at most the allowed age old. `age_seconds` is whole seconds.
    Alive { age_seconds: u64 },
    /// The newest heartbeat is older than allowed.
    Stale { age_seconds: u64 },
    /// The source has never written a heartbeat.
    Never,
    /// The newest heartbeat is in the future by more than the allowed age: a clock is wrong,
    /// which is as bad as silence.
    ClockSkew { ahead_seconds: u64 },
}

impl Liveness {
    /// True only for [`Liveness::Alive`].
    #[must_use]
    pub const fn is_alive(&self) -> bool {
        matches!(self, Self::Alive { .. })
    }
}

/// Pure: `latest` is the newest heartbeat time, `now` the checker's clock. A heartbeat in the
/// future by no more than `max_age_seconds` counts as `Alive { age_seconds: 0 }`. Ages are
/// whole seconds; `Alive` iff the age in nanoseconds is at most `max_age_seconds` seconds.
#[must_use]
pub fn judge(latest: Option<UnixNanos>, now: UnixNanos, max_age_seconds: u32) -> Liveness {
    let Some(latest) = latest else {
        return Liveness::Never;
    };
    let limit = u64::from(max_age_seconds) * NANOS_PER_SECOND;
    let (latest, now) = (latest.as_u64(), now.as_u64());
    if latest > now {
        let ahead = latest - now;
        return if ahead <= limit {
            Liveness::Alive { age_seconds: 0 }
        } else {
            Liveness::ClockSkew {
                ahead_seconds: ahead / NANOS_PER_SECOND,
            }
        };
    }
    let age = now - latest;
    let age_seconds = age / NANOS_PER_SECOND;
    if age <= limit {
        Liveness::Alive { age_seconds }
    } else {
        Liveness::Stale { age_seconds }
    }
}

/// Reads the newest heartbeat of `source` and judges it. One indexed query.
pub async fn check<C: GenericClient>(
    client: &C,
    source: &str,
    now: UnixNanos,
    max_age_seconds: u32,
) -> Result<Liveness, EvalError> {
    let latest = heartbeat::latest(client, source).await?;
    Ok(judge(latest, now, max_age_seconds))
}
