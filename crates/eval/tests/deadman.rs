//! Acceptance tests for the dead-man check: `judge` as pure values, and `check` against a real
//! Postgres (`TEST_DATABASE_URL`).

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "integration test")]

use nisse_core::nautilus::UnixNanos;
use nisse_eval::deadman::{Liveness, check, judge};
use nisse_store::{tables::heartbeat, testing::ScratchDb};

const SECOND: u64 = 1_000_000_000;
/// 2026-01-05T09:46:40.123456Z. A whole number of microseconds, so a round trip through
/// `TIMESTAMPTZ` keeps it exact.
const T: u64 = 1_767_600_000_123_456_000;

fn at(nanos: u64) -> UnixNanos {
    UnixNanos::from(nanos)
}

// ---- judge ----------------------------------------------------------------------------------

#[test]
fn no_heartbeat_is_never() {
    assert_eq!(judge(None, at(T), 300), Liveness::Never);
}

#[test]
fn a_heartbeat_now_is_alive_at_age_zero() {
    assert_eq!(
        judge(Some(at(T)), at(T), 300),
        Liveness::Alive { age_seconds: 0 }
    );
}

#[test]
fn an_age_of_exactly_the_limit_is_alive() {
    assert_eq!(
        judge(Some(at(T)), at(T + 300 * SECOND), 300),
        Liveness::Alive { age_seconds: 300 }
    );
}

#[test]
fn one_nanosecond_past_the_limit_is_stale() {
    assert_eq!(
        judge(Some(at(T)), at(T + 300 * SECOND + 1), 300),
        Liveness::Stale { age_seconds: 300 }
    );
}

#[test]
fn an_old_heartbeat_is_stale_with_its_age() {
    assert_eq!(
        judge(Some(at(T)), at(T + 900 * SECOND), 300),
        Liveness::Stale { age_seconds: 900 }
    );
}

#[test]
fn a_heartbeat_slightly_in_the_future_is_alive_at_age_zero() {
    assert_eq!(
        judge(Some(at(T + 10 * SECOND)), at(T), 300),
        Liveness::Alive { age_seconds: 0 }
    );
}

#[test]
fn a_heartbeat_far_in_the_future_is_clock_skew() {
    assert_eq!(
        judge(Some(at(T + 301 * SECOND)), at(T), 300),
        Liveness::ClockSkew { ahead_seconds: 301 }
    );
}

#[test]
fn only_alive_is_alive() {
    assert!(Liveness::Alive { age_seconds: 0 }.is_alive());
    assert!(!Liveness::Stale { age_seconds: 1 }.is_alive());
    assert!(!Liveness::Never.is_alive());
    assert!(!Liveness::ClockSkew { ahead_seconds: 1 }.is_alive());
}

// ---- check ----------------------------------------------------------------------------------

#[tokio::test]
async fn check_reports_never_for_a_source_with_no_heartbeat() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();

    assert_eq!(
        check(&client, "engine", at(T), 300).await.unwrap(),
        Liveness::Never
    );
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn check_judges_the_newest_heartbeat_of_the_source() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    heartbeat::insert(&client, "engine", at(T)).await.unwrap();

    assert_eq!(
        check(&client, "engine", at(T + 100 * SECOND), 300)
            .await
            .unwrap(),
        Liveness::Alive { age_seconds: 100 }
    );
    assert_eq!(
        check(&client, "engine", at(T + 301 * SECOND), 300)
            .await
            .unwrap(),
        Liveness::Stale { age_seconds: 301 }
    );
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn another_sources_fresh_heartbeat_does_not_make_engine_alive() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    heartbeat::insert(&client, "engine", at(T)).await.unwrap();
    heartbeat::insert(&client, "cron", at(T + 301 * SECOND))
        .await
        .unwrap();

    assert_eq!(
        check(&client, "engine", at(T + 301 * SECOND), 300)
            .await
            .unwrap(),
        Liveness::Stale { age_seconds: 301 }
    );
    assert_eq!(
        check(&client, "cron", at(T + 301 * SECOND), 300)
            .await
            .unwrap(),
        Liveness::Alive { age_seconds: 0 }
    );
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn check_judges_the_newest_of_several_heartbeats_not_the_last_written() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    // Written out of time order: the newest by time is T + 200 s, written second.
    heartbeat::insert(&client, "engine", at(T + 200 * SECOND))
        .await
        .unwrap();
    heartbeat::insert(&client, "engine", at(T + 50 * SECOND))
        .await
        .unwrap();

    assert_eq!(
        check(&client, "engine", at(T + 300 * SECOND), 300)
            .await
            .unwrap(),
        Liveness::Alive { age_seconds: 100 }
    );
    db.drop_db().await.unwrap();
}
