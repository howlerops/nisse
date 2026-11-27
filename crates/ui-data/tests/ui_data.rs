#![allow(clippy::unwrap_used, clippy::expect_used, reason = "integration test")]

//! Acceptance tests U2: the phone UI's read functions and the kill call, against a real
//! Postgres (`TEST_DATABASE_URL`; run `eval "$(scripts/dev-postgres.sh)"` first).

use std::collections::BTreeMap;

use nisse_core::nautilus::{
    ClientOrderId, OrderSide, OrderType, Price, Quantity, Symbol, TimeInForce, UnixNanos,
    VenueOrderId,
};
use nisse_core::{
    Action, Decision, DecisionId, EvalScore, Hash32, OrderEvent, OrderIntent, OrderState,
    PositionEvent, RiskEvent, RiskEventKind, StrategyVersion, SubjectType,
};
use nisse_store::{
    tables::{
        decision,
        engine_step::{self, StepStatus},
        eval_score, order_event, position_event, risk_event, strategy_version,
    },
    testing::ScratchDb,
};
use nisse_ui_data::{
    GateState, RESIZE_RUN_KEY, RESIZE_STEP, UiError, engage_kill, open_orders, positions,
    recent_steps, status, versions,
};
use rust_decimal::Decimal;
use serde_json::{Value, json};
use tokio_postgres::{Client, error::SqlState};

/// 2026-01-05T09:46:40.123456Z, a whole number of microseconds (the store keeps microseconds).
const T0: u64 = 1_767_600_000_123_456_000;
const SECOND: u64 = 1_000_000_000;
const SOURCE: &str = "engine";

fn dec(text: &str) -> Decimal {
    text.parse().unwrap()
}

fn at(nanos: u64) -> UnixNanos {
    UnixNanos::from(nanos)
}

fn risk(kind: RiskEventKind, detail: Value, ts: u64) -> RiskEvent {
    RiskEvent {
        kind,
        detail,
        ts: at(ts),
    }
}

fn position(symbol: &str, qty: &str, cost: &str, ts: u64) -> PositionEvent {
    PositionEvent {
        symbol: Symbol::new_checked(symbol).unwrap(),
        qty_after: dec(qty),
        avg_cost: dec(cost),
        ts: at(ts),
    }
}

/// A connection that acts as the phone's role, `nisse_ui`.
async fn ui_client(db: &ScratchDb) -> Client {
    let client = db.client().await.unwrap();
    client.batch_execute("SET ROLE nisse_ui").await.unwrap();
    client
}

/// A version with a parameter set that differs by `tag`, so two versions have two hashes.
fn version(git: char, tag: u64) -> StrategyVersion {
    let params = json!({"strategy": "threshold", "variant": tag});
    StrategyVersion {
        git_sha: git.to_string().repeat(40),
        config_hash: nisse_core::config_hash(&params),
        params,
        parent_id: None,
        ts: at(T0),
    }
}

/// A decision for `version_id` on SPY; returns its id.
async fn decision_id(client: &Client, version_id: nisse_core::StrategyVersionId) -> DecisionId {
    let decision = Decision {
        strategy_version_id: version_id,
        symbol: Symbol::new("SPY"),
        action: Action::Enter,
        intended_qty: dec("5"),
        expected_edge_bps: None,
        expected_vol: None,
        features: BTreeMap::new(),
        feature_hash: Hash32::from_bytes([0xab; 32]),
        data_snapshot_id: "snap-1".to_string(),
        rng_seed: 42,
        reason_code: "trend_up".to_string(),
        ts: at(T0),
    };
    decision::insert(client, &decision).await.unwrap()
}

/// A limit buy of 5 SPY at 500.10 made by `decision`.
fn intent(decision: DecisionId, client_order_id: &str, ts: u64) -> OrderIntent {
    OrderIntent {
        decision_id: decision,
        client_order_id: ClientOrderId::new_checked(client_order_id).unwrap(),
        symbol: Symbol::new_checked("SPY").unwrap(),
        side: OrderSide::Buy,
        order_type: OrderType::Limit,
        qty: Quantity::from("5"),
        limit_px: Some("500.10".parse::<Price>().unwrap()),
        time_in_force: TimeInForce::Day,
        ts: at(ts),
    }
}

fn state_row(intent: &OrderIntent, state: OrderState, venue: Option<&str>, ts: u64) -> OrderEvent {
    OrderEvent::of(
        intent,
        state,
        venue.map(VenueOrderId::from),
        Quantity::from("0"),
        at(ts),
    )
}

fn gate_score(subject_id: i64, metric: &str, value: f64, ts: u64) -> EvalScore {
    EvalScore {
        subject_id,
        subject_type: SubjectType::StrategyVersion,
        metric: metric.to_string(),
        value,
        eval_version: "test-eval".to_string(),
        ts: at(ts),
    }
}

// ---- status ------------------------------------------------------------------------------

#[tokio::test]
async fn status_of_an_empty_store() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let got = status(&client, SOURCE).await.unwrap();
    assert_eq!(
        got,
        nisse_ui_data::Status {
            kill_switch: false,
            drawdown_mode: "normal".to_string(),
            equity_peak: None,
            last_heartbeat: None,
            heartbeat_age_seconds: None,
            watcher_age_seconds: None,
            rebalance_owed: false,
            limits_hash: None,
        }
    );
}

#[tokio::test]
async fn status_reflects_what_is_stored() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    risk_event::insert_many(
        &client,
        &[
            risk(RiskEventKind::EquityPeak, json!({"peak": "101250.75"}), T0),
            risk(
                RiskEventKind::DrawdownMode,
                json!({"mode": "halved", "equity": "92000.10", "reset": false}),
                T0 + SECOND,
            ),
            risk(
                RiskEventKind::LimitsLoaded,
                json!({"hash": "ab12cd34", "limits": {"max_position": 100}}),
                T0 + 2 * SECOND,
            ),
        ],
    )
    .await
    .unwrap();
    // Ninety seconds ago by the database's own clock, which is the clock the age is taken on.
    client
        .execute(
            "INSERT INTO heartbeat (source, event_ts) \
             VALUES ($1, clock_timestamp() - interval '90 seconds')",
            &[&SOURCE],
        )
        .await
        .unwrap();
    engine_step::insert(
        &client,
        RESIZE_RUN_KEY,
        RESIZE_STEP,
        StepStatus::Started,
        &json!({}),
        at(T0 + 3 * SECOND),
    )
    .await
    .unwrap();

    let got = status(&client, SOURCE).await.unwrap();
    assert!(!got.kill_switch);
    assert_eq!(got.drawdown_mode, "halved");
    assert_eq!(got.equity_peak, Some(dec("101250.75")));
    assert!(got.last_heartbeat.is_some());
    let age = got.heartbeat_age_seconds.unwrap();
    assert!((90..=95).contains(&age), "{age}");
    assert!(got.rebalance_owed);
    assert_eq!(got.limits_hash.as_deref(), Some("ab12cd34"));

    // A heartbeat a second or two ahead of the database is rounding: age zero.
    client
        .execute(
            "INSERT INTO heartbeat (source, event_ts) \
             VALUES ($1, clock_timestamp() + interval '2 seconds')",
            &[&SOURCE],
        )
        .await
        .unwrap();
    let got = status(&client, SOURCE).await.unwrap();
    assert_eq!(got.heartbeat_age_seconds, Some(0));

    // One well in the future means a clock is wrong: the liveness cannot be judged, and that
    // is an error, never "just now".
    client
        .execute(
            "INSERT INTO heartbeat (source, event_ts) \
             VALUES ($1, clock_timestamp() + interval '20 minutes')",
            &[&SOURCE],
        )
        .await
        .unwrap();
    let err = status(&client, SOURCE).await.unwrap_err();
    assert!(matches!(err, UiError::Invalid(_)), "{err:?}");
}

#[tokio::test]
async fn a_resized_step_that_is_done_owes_nothing() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    engine_step::insert(
        &client,
        RESIZE_RUN_KEY,
        RESIZE_STEP,
        StepStatus::Started,
        &json!({}),
        at(T0),
    )
    .await
    .unwrap();
    engine_step::insert(
        &client,
        RESIZE_RUN_KEY,
        RESIZE_STEP,
        StepStatus::Done,
        &json!({}),
        at(T0 + SECOND),
    )
    .await
    .unwrap();
    assert!(!status(&client, SOURCE).await.unwrap().rebalance_owed);
}

#[tokio::test]
async fn status_fails_on_an_unreadable_risk_event() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    risk_event::insert_many(
        &client,
        &[risk(
            RiskEventKind::DrawdownMode,
            json!({"mode": "sideways"}),
            T0,
        )],
    )
    .await
    .unwrap();
    let err = status(&client, SOURCE).await.unwrap_err();
    assert!(matches!(err, UiError::Risk(_)), "got {err:?}");
}

// ---- positions ---------------------------------------------------------------------------

#[tokio::test]
async fn positions_are_the_newest_per_symbol_without_flat_ones() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    position_event::insert_many(
        &client,
        &[
            position("AAPL", "5", "100.5", T0),
            position("MSFT", "2", "50", T0),
            position("AAPL", "7", "101", T0 + SECOND),
            position("SPY", "3", "450", T0),
            // The newest SPY row is flat, so SPY is left out even though an older row is not.
            position("SPY", "0", "0", T0 + SECOND),
            position("ZZZ", "1", "10", T0),
        ],
    )
    .await
    .unwrap();
    let got = positions(&client).await.unwrap();
    let summary: Vec<(String, Decimal, Decimal)> = got
        .iter()
        .map(|p| (p.symbol.as_str().to_string(), p.qty_after, p.avg_cost))
        .collect();
    assert_eq!(
        summary,
        vec![
            ("AAPL".to_string(), dec("7"), dec("101")),
            ("MSFT".to_string(), dec("2"), dec("50")),
            ("ZZZ".to_string(), dec("1"), dec("10")),
        ]
    );
}

// ---- open orders -------------------------------------------------------------------------

#[tokio::test]
async fn open_orders_lists_only_orders_that_are_not_final() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let v = strategy_version::ensure(&client, &version('a', 1))
        .await
        .unwrap();
    let d = decision_id(&client, v).await;

    let a = intent(d, "O-A", T0);
    let b = intent(d, "O-B", T0);
    let c = intent(d, "O-C", T0);
    order_event::insert(&client, &state_row(&a, OrderState::Intent, None, T0))
        .await
        .unwrap();
    order_event::insert(
        &client,
        &state_row(&a, OrderState::Accepted, Some("V-A"), T0 + SECOND),
    )
    .await
    .unwrap();
    order_event::insert(&client, &state_row(&b, OrderState::Intent, None, T0))
        .await
        .unwrap();
    order_event::insert(
        &client,
        &state_row(&b, OrderState::Canceled, Some("V-B"), T0 + SECOND),
    )
    .await
    .unwrap();
    order_event::insert(&client, &state_row(&c, OrderState::Intent, None, T0))
        .await
        .unwrap();

    let got = open_orders(&client).await.unwrap();
    assert_eq!(got.len(), 2, "{got:?}");
    let first = &got[0];
    assert_eq!(first.client_order_id, "O-A");
    assert_eq!(first.symbol, "SPY");
    assert_eq!(first.side, OrderSide::Buy.to_string());
    assert_eq!(first.state, "accepted");
    assert_eq!(first.qty, dec("5"));
    assert_eq!(first.limit_px, Some(dec("500.10")));
    assert_eq!(first.last_change, at(T0 + SECOND));
    assert_eq!(got[1].client_order_id, "O-C");
    assert_eq!(got[1].state, "intent");
}

// ---- versions ----------------------------------------------------------------------------

#[tokio::test]
async fn versions_show_the_newest_status_of_each_gate() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let older = strategy_version::ensure(&client, &version('a', 1))
        .await
        .unwrap();
    let newer = strategy_version::ensure(&client, &version('b', 2))
        .await
        .unwrap();

    // dsr fails first, then passes: the newer row wins. pbo was not evaluated. sharpe is not a
    // gate and must not appear.
    eval_score::insert_many(&client, &[gate_score(older.0, "gate_dsr_status", 0.0, T0)])
        .await
        .unwrap();
    eval_score::insert_many(
        &client,
        &[gate_score(older.0, "gate_dsr_status", 1.0, T0 + SECOND)],
    )
    .await
    .unwrap();
    eval_score::insert_many(
        &client,
        &[
            gate_score(older.0, "gate_pbo_status", -1.0, T0),
            gate_score(older.0, "sharpe", 0.5, T0),
        ],
    )
    .await
    .unwrap();

    let got = versions(&client).await.unwrap();
    assert_eq!(got.len(), 2, "{got:?}");
    assert_eq!(got[0].strategy_version_id, newer.0);
    assert_eq!(got[0].git_sha, "b".repeat(40));
    assert_eq!(got[0].config_hash.len(), 64);
    assert!(got[0].gates.is_empty());
    assert_eq!(got[1].strategy_version_id, older.0);
    assert_eq!(got[1].git_sha, "a".repeat(40));
    assert_eq!(
        got[1].gates,
        vec![
            ("dsr".to_string(), GateState::Pass),
            ("pbo".to_string(), GateState::NotEvaluated),
        ]
    );
}

#[tokio::test]
async fn a_gate_that_failed_reads_as_fail() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let v = strategy_version::ensure(&client, &version('a', 1))
        .await
        .unwrap();
    eval_score::insert_many(&client, &[gate_score(v.0, "gate_holdout_status", 0.0, T0)])
        .await
        .unwrap();
    let got = versions(&client).await.unwrap();
    assert_eq!(got[0].gates, vec![("holdout".to_string(), GateState::Fail)]);
}

// ---- recent steps ------------------------------------------------------------------------

#[tokio::test]
async fn recent_steps_are_newest_first_and_clamped() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    for i in 0..205_u64 {
        engine_step::insert(
            &client,
            "tick/2026-01-05",
            "ingest",
            StepStatus::Done,
            &json!({"i": i}),
            at(T0 + i * SECOND),
        )
        .await
        .unwrap();
    }

    // limit 0 is clamped up to one row, the newest.
    let got = recent_steps(&client, 0).await.unwrap();
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].detail, json!({"i": 204}));
    assert_eq!(got[0].status, "done");
    assert_eq!(got[0].run_key, "tick/2026-01-05");
    assert_eq!(got[0].step, "ingest");
    assert_eq!(got[0].ts, at(T0 + 204 * SECOND));

    // limit 1000 is clamped down to 200 rows, still newest first.
    let got = recent_steps(&client, 1000).await.unwrap();
    assert_eq!(got.len(), 200);
    assert_eq!(got[0].detail, json!({"i": 204}));
    assert_eq!(got[199].detail, json!({"i": 5}));
}

// ---- kill switch -------------------------------------------------------------------------

#[tokio::test]
async fn engage_kill_writes_and_confirms() {
    let db = ScratchDb::create().await;
    let owner = db.client().await.unwrap();
    let ui = ui_client(&db).await;

    let id = engage_kill(&ui, "  ana ").await.unwrap();
    let got = status(&ui, SOURCE).await.unwrap();
    assert!(got.kill_switch);

    let detail: Value = owner
        .query_one("SELECT detail FROM risk_event WHERE id = $1", &[&id])
        .await
        .unwrap()
        .get(0);
    assert_eq!(detail["engaged"], json!(true));
    assert_eq!(detail["by"], json!("ana"));
    assert_eq!(detail["via"], json!("ui"));
}

#[tokio::test]
async fn the_ui_role_cannot_write_anything_else() {
    let db = ScratchDb::create().await;
    let owner = db.client().await.unwrap();
    let ui = ui_client(&db).await;

    let err = ui
        .execute(
            "INSERT INTO risk_event (kind, detail, event_ts) \
             VALUES ('kill_switch', '{\"engaged\": false}', now())",
            &[],
        )
        .await
        .unwrap_err();
    assert_eq!(err.code(), Some(&SqlState::INSUFFICIENT_PRIVILEGE));

    let err = ui
        .execute(
            "INSERT INTO heartbeat (source, event_ts) VALUES ('ui', now())",
            &[],
        )
        .await
        .unwrap_err();
    assert_eq!(err.code(), Some(&SqlState::INSUFFICIENT_PRIVILEGE));

    // Nothing was written by either attempt.
    let count: i64 = owner
        .query_one("SELECT count(*) FROM heartbeat", &[])
        .await
        .unwrap()
        .get(0);
    assert_eq!(count, 0);

    // The read functions all work under the role.
    status(&ui, SOURCE).await.unwrap();
    positions(&ui).await.unwrap();
    open_orders(&ui).await.unwrap();
    versions(&ui).await.unwrap();
    recent_steps(&ui, 10).await.unwrap();
}

#[tokio::test]
async fn engage_kill_refuses_a_bad_name_before_sending() {
    let db = ScratchDb::create().await;
    let owner = db.client().await.unwrap();
    let ui = ui_client(&db).await;

    let too_long = "x".repeat(65);
    for name in ["", "   ", too_long.as_str()] {
        let err = engage_kill(&ui, name).await.unwrap_err();
        assert!(matches!(err, UiError::Invalid(_)), "{name:?}: {err:?}");
    }
    let count: i64 = owner
        .query_one(
            "SELECT count(*) FROM risk_event WHERE kind = 'kill_switch'",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(count, 0);

    // Exactly 64 characters is allowed.
    let longest = "y".repeat(64);
    engage_kill(&ui, &longest).await.unwrap();
}

#[tokio::test]
async fn a_release_after_a_ui_kill_is_seen() {
    let db = ScratchDb::create().await;
    let owner = db.client().await.unwrap();
    let ui = ui_client(&db).await;

    engage_kill(&ui, "ana").await.unwrap();
    assert!(status(&ui, SOURCE).await.unwrap().kill_switch);

    risk_event::insert_many(
        &owner,
        &[risk(
            RiskEventKind::KillSwitch,
            json!({"engaged": false, "by": "owner"}),
            T0 + SECOND,
        )],
    )
    .await
    .unwrap();
    assert!(!status(&ui, SOURCE).await.unwrap().kill_switch);
}

// ---- constants ---------------------------------------------------------------------------

#[test]
fn the_resize_constants_still_appear_in_the_engine_session() {
    // Origin of the constants: crates/app/src/session.rs. A rename there must fail here.
    let session = include_str!("../../app/src/session.rs");
    assert!(
        session.contains(&format!("RESIZE_RUN_KEY: &str = \"{RESIZE_RUN_KEY}\"")),
        "session.rs no longer defines RESIZE_RUN_KEY as {RESIZE_RUN_KEY:?}"
    );
    assert!(
        session.contains(&format!("RESIZE_STEP: &str = \"{RESIZE_STEP}\"")),
        "session.rs no longer defines RESIZE_STEP as {RESIZE_STEP:?}"
    );
}

#[tokio::test]
async fn the_watcher_has_its_own_age_and_steps_carry_theirs() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    client
        .execute(
            "INSERT INTO heartbeat (source, event_ts) VALUES \
             ($1, clock_timestamp() - interval '40 seconds'), \
             ('engine', clock_timestamp() - interval '200 seconds')",
            &[&nisse_ui_data::WATCH_SOURCE],
        )
        .await
        .unwrap();
    client
        .execute(
            "INSERT INTO engine_step (run_key, step, status, detail, event_ts) \
             VALUES ('kill', 'cancel', 'done', '{}', clock_timestamp() - interval '3 hours')",
            &[],
        )
        .await
        .unwrap();

    let got = status(&client, SOURCE).await.unwrap();
    let watcher = got.watcher_age_seconds.unwrap();
    assert!((40..=45).contains(&watcher), "{watcher}");
    let engine = got.heartbeat_age_seconds.unwrap();
    assert!((200..=205).contains(&engine), "{engine}");

    let steps = recent_steps(&client, 10).await.unwrap();
    assert_eq!(steps.len(), 1);
    assert!(
        (10_800..=10_810).contains(&steps[0].age_seconds),
        "{:?}",
        steps[0]
    );

    // The watcher's source name is the engine's own constant.
    assert!(
        include_str!("../../app/src/kill_watch.rs")
            .contains(&format!("\"{}\"", nisse_ui_data::WATCH_SOURCE))
    );

    drop(client);
    db.drop_db().await.unwrap();
}
