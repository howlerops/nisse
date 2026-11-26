//! The awaited write path end to end: a decision, then the write-ahead intent for an order
//! that refers to it, then what a restart would load.
#![allow(clippy::unwrap_used, clippy::expect_used, reason = "integration test")]

use nisse_core::nautilus::{
    ClientOrderId, OrderSide, OrderType, Price, Quantity, Symbol, TimeInForce, UnixNanos,
};
use nisse_core::{
    Action, Decision, DecisionLog, Features, IntentLog, OrderIntent, OrderState, StrategyVersion,
    config_hash, feature_hash,
};
use nisse_store::{Store, StoreError, testing::ScratchDb};
use serde_json::json;

fn ts(micros: u64) -> UnixNanos {
    UnixNanos::from(1_767_600_000_000_000_000 + micros * 1_000)
}

#[tokio::test]
async fn decision_then_intent_then_restart_load() {
    let db = ScratchDb::create().await;
    let store = Store::new(db.client().await.unwrap());

    let params = json!({"strategy": "threshold", "level": "1"});
    let version = StrategyVersion {
        git_sha: "a".repeat(40),
        config_hash: config_hash(&params),
        params,
        parent_id: None,
        ts: ts(0),
    };
    let version_id = store.ensure_strategy_version(&version).await.unwrap();
    assert_eq!(
        store.ensure_strategy_version(&version).await.unwrap(),
        version_id
    );

    let mut features = Features::new();
    features.insert("signal".to_owned(), "1.5".parse().unwrap());
    let decision = Decision {
        strategy_version_id: version_id,
        symbol: Symbol::new("SPY"),
        action: Action::Enter,
        intended_qty: "1.5".parse().unwrap(),
        expected_edge_bps: Some(12.5),
        expected_vol: None,
        feature_hash: feature_hash(&features),
        features,
        data_snapshot_id: "snap-1".to_owned(),
        rng_seed: 7,
        reason_code: "above".to_owned(),
        ts: ts(1),
    };
    let decision_id = store.record_decision(&decision).await.unwrap();

    let intent = OrderIntent {
        decision_id,
        client_order_id: ClientOrderId::new("nisse-1-0"),
        symbol: Symbol::new("SPY"),
        side: OrderSide::Buy,
        order_type: OrderType::Limit,
        qty: Quantity::from("1.5"),
        limit_px: Some("500.10".parse::<Price>().unwrap()),
        time_in_force: TimeInForce::Day,
        ts: ts(2),
    };
    store.record_intent(&intent).await.unwrap();

    // A restart with nothing but the store finds the order, still only an intent.
    let open = store.load_open_orders().await.unwrap();
    assert_eq!(open.len(), 1);
    assert_eq!(open[0].intent, intent);
    assert_eq!(open[0].state, OrderState::Intent);
    assert_eq!(open[0].venue_order_id, None);
    assert_eq!(open[0].avg_fill_px, None);
    assert!(open[0].cum_filled_qty.is_zero());

    // The same client order id cannot be intended twice in a way that loses the first row:
    // a second intent row is simply another row, and the first is still the intent.
    // An intent for a decision that does not exist is refused by the database.
    let mut orphan = intent;
    orphan.client_order_id = ClientOrderId::new("nisse-2-0");
    orphan.decision_id = nisse_core::DecisionId(999_999);
    assert!(matches!(
        store.record_intent(&orphan).await,
        Err(StoreError::Db(_))
    ));

    db.drop_db().await.unwrap();
}
