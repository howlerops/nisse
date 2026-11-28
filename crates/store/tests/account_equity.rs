//! Acceptance tests for `account_equity` against a real Postgres (`TEST_DATABASE_URL`).

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "integration test")]

use chrono::NaiveDate;
use nisse_core::{StrategyVersion, nautilus::UnixNanos};
use nisse_store::{
    StoreError,
    tables::{account_equity, strategy_version},
    testing::ScratchDb,
};
use rust_decimal::Decimal;
use serde_json::json;

/// 2026-01-05T09:46:40.123456Z, a whole number of microseconds.
const T0: u64 = 1_767_600_000_123_456_000;

fn dec(text: &str) -> Decimal {
    text.parse().unwrap()
}

fn day(d: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 1, d).unwrap()
}

fn is_db_error(err: &StoreError) -> bool {
    matches!(err, StoreError::Db(_))
}

/// A version at git SHA `git` (a single hex digit repeated) with the config `variant`. Two
/// versions with the same `variant` share a config hash whatever their git SHA.
fn version(git: char, variant: i64) -> StrategyVersion {
    let params = json!({"strategy": "threshold", "lookback": 252, "variant": variant});
    StrategyVersion {
        git_sha: git.to_string().repeat(40),
        config_hash: nisse_core::config_hash(&params),
        params,
        parent_id: None,
        ts: UnixNanos::from(T0),
    }
}

#[tokio::test]
async fn daily_is_empty_without_rows() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let id = strategy_version::ensure(&client, &version('a', 1))
        .await
        .unwrap();

    assert!(
        account_equity::daily(&client, id, "paper")
            .await
            .unwrap()
            .is_empty()
    );
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn daily_returns_sessions_in_date_order() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let id = strategy_version::ensure(&client, &version('a', 1))
        .await
        .unwrap();
    let ts = UnixNanos::from(T0);

    // Inserted out of order.
    account_equity::insert(&client, id, "paper", day(7), dec("100250.50"), ts)
        .await
        .unwrap();
    account_equity::insert(&client, id, "paper", day(5), dec("100000.25"), ts)
        .await
        .unwrap();
    account_equity::insert(&client, id, "paper", day(6), dec("99999.75"), ts)
        .await
        .unwrap();

    assert_eq!(
        account_equity::daily(&client, id, "paper").await.unwrap(),
        vec![
            (day(5), dec("100000.25")),
            (day(6), dec("99999.75")),
            (day(7), dec("100250.50")),
        ]
    );
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn the_newest_row_of_a_session_wins() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let id = strategy_version::ensure(&client, &version('a', 1))
        .await
        .unwrap();
    let ts = UnixNanos::from(T0);

    account_equity::insert(&client, id, "paper", day(5), dec("100000.25"), ts)
        .await
        .unwrap();
    account_equity::insert(&client, id, "paper", day(5), dec("100010.00"), ts)
        .await
        .unwrap();

    assert_eq!(
        account_equity::daily(&client, id, "paper").await.unwrap(),
        vec![(day(5), dec("100010.00"))]
    );
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn environments_are_separate() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let id = strategy_version::ensure(&client, &version('a', 1))
        .await
        .unwrap();
    let ts = UnixNanos::from(T0);

    account_equity::insert(&client, id, "paper", day(5), dec("100000"), ts)
        .await
        .unwrap();
    account_equity::insert(&client, id, "live", day(5), dec("5000"), ts)
        .await
        .unwrap();
    account_equity::insert(&client, id, "live", day(6), dec("5010.5"), ts)
        .await
        .unwrap();

    assert_eq!(
        account_equity::daily(&client, id, "paper").await.unwrap(),
        vec![(day(5), dec("100000"))]
    );
    assert_eq!(
        account_equity::daily(&client, id, "live").await.unwrap(),
        vec![(day(5), dec("5000")), (day(6), dec("5010.5"))]
    );
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn a_new_build_of_the_same_config_continues_the_series() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let old = strategy_version::ensure(&client, &version('a', 1))
        .await
        .unwrap();
    let new = strategy_version::ensure(&client, &version('b', 1))
        .await
        .unwrap();
    assert_ne!(old, new, "different git SHAs are different versions");
    let ts = UnixNanos::from(T0);

    account_equity::insert(&client, old, "paper", day(5), dec("100000"), ts)
        .await
        .unwrap();
    account_equity::insert(&client, new, "paper", day(6), dec("100100"), ts)
        .await
        .unwrap();

    let expected = vec![(day(5), dec("100000")), (day(6), dec("100100"))];
    assert_eq!(
        account_equity::daily(&client, old, "paper").await.unwrap(),
        expected
    );
    assert_eq!(
        account_equity::daily(&client, new, "paper").await.unwrap(),
        expected
    );
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn another_config_is_not_included() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let mine = strategy_version::ensure(&client, &version('a', 1))
        .await
        .unwrap();
    let other = strategy_version::ensure(&client, &version('a', 2))
        .await
        .unwrap();
    let ts = UnixNanos::from(T0);

    account_equity::insert(&client, mine, "paper", day(5), dec("100000"), ts)
        .await
        .unwrap();
    account_equity::insert(&client, other, "paper", day(5), dec("777"), ts)
        .await
        .unwrap();
    account_equity::insert(&client, other, "paper", day(6), dec("888"), ts)
        .await
        .unwrap();

    assert_eq!(
        account_equity::daily(&client, mine, "paper").await.unwrap(),
        vec![(day(5), dec("100000"))]
    );
    assert_eq!(
        account_equity::daily(&client, other, "paper")
            .await
            .unwrap(),
        vec![(day(5), dec("777")), (day(6), dec("888"))]
    );
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn an_unknown_environment_is_a_db_error() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let id = strategy_version::ensure(&client, &version('a', 1))
        .await
        .unwrap();

    let err = account_equity::insert(
        &client,
        id,
        "shadow",
        day(5),
        dec("100000"),
        UnixNanos::from(T0),
    )
    .await
    .unwrap_err();
    assert!(is_db_error(&err), "got {err:?}");
    assert!(
        account_equity::daily(&client, id, "shadow")
            .await
            .unwrap()
            .is_empty()
    );
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn a_negative_equity_is_a_db_error() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let id = strategy_version::ensure(&client, &version('a', 1))
        .await
        .unwrap();

    let err = account_equity::insert(
        &client,
        id,
        "paper",
        day(5),
        dec("-0.01"),
        UnixNanos::from(T0),
    )
    .await
    .unwrap_err();
    assert!(is_db_error(&err), "got {err:?}");
    assert!(
        account_equity::daily(&client, id, "paper")
            .await
            .unwrap()
            .is_empty()
    );
    db.drop_db().await.unwrap();
}
