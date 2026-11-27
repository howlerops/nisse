//! `App.tsx` against the crate's module and a real Postgres, headless: the TSX entry is
//! bundled and run on QuickJS as `nsh run` does, presses go through its UI, and the store is
//! a scratch database (`TEST_DATABASE_URL`, as for the rest of the repository's tests). The
//! app connects as the role `nisse_ui`, the one a phone's login would be a member of.

use std::sync::{Arc, OnceLock};

use nisse_core::PositionEvent;
use nisse_core::nautilus::{Symbol, UnixNanos};

use nisse_store::testing::ScratchDb;
use nsh_core::element::Registry;
use nsh_engine::{Bundle, EngineConfig};
use nsh_testing::{Headless, MemoryStorage};

/// The key the app keeps its connection string under (`URL_KEY` in `App.tsx`).
const URL_KEY: &str = "nisse.database_url";

fn ui_sets() -> Vec<Registry> {
    vec![nsh_base::registry(), nisse_native::registry()]
}

/// `App.tsx` as bytecode, built once for every test.
fn app() -> Bundle {
    static APP: OnceLock<Bundle> = OnceLock::new();
    APP.get_or_init(|| {
        let entry = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../App.tsx");
        nsh_testing::compile_tsx(entry, &ui_sets()).expect("App.tsx builds")
    })
    .clone()
}

/// The app with `url` saved as its connection (or none), once it shows `ready`.
fn start(url: Option<&str>, ready: &str) -> Headless {
    start_with_storage(url, ready).0
}

/// As [`start`], also returning the app's storage so a test can see what the app saved.
fn start_with_storage(url: Option<&str>, ready: &str) -> (Headless, Arc<MemoryStorage>) {
    let entries = url.map(|url| (URL_KEY.to_owned(), url.to_owned()));
    let storage = MemoryStorage::new(entries);
    let mut app = Headless::start(EngineConfig {
        ui_sets: ui_sets(),
        storage: Some(storage.clone()),
        ..EngineConfig::new(app())
    })
    .expect("the engine starts");
    app.wait_for(ready).unwrap();
    (app, storage)
}

/// As [`start`], with a clock the test moves (`fast_forward`): timers fire only when it does.
fn start_with_manual_clock(url: &str, ready: &str) -> Headless {
    let mut app = Headless::start(EngineConfig {
        ui_sets: ui_sets(),
        storage: Some(MemoryStorage::new([(URL_KEY.to_owned(), url.to_owned())])),
        clock: nsh_core::time::Clock::manual(std::time::SystemTime::now()),
        ..EngineConfig::new(app())
    })
    .expect("the engine starts");
    app.wait_for(ready).unwrap();
    app
}

/// A runtime for the test's own database work (the app has the engine's).
fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap()
}

/// A scratch store in which the engine and the kill watcher each wrote a heartbeat a few
/// seconds ago.
fn store_with_a_live_engine(rt: &tokio::runtime::Runtime) -> ScratchDb {
    let db = rt.block_on(ScratchDb::create());
    let client = rt.block_on(db.client()).unwrap();
    rt.block_on(client.execute(
        "INSERT INTO heartbeat (source, event_ts) VALUES \
         ('engine', clock_timestamp() - interval '5 seconds'), \
         ('kill-watch', clock_timestamp() - interval '5 seconds')",
        &[],
    ))
    .unwrap();
    db
}

/// `url` with the session's role set to `nisse_ui`, as a phone's login would act.
fn as_ui_role(url: &str) -> String {
    let separator = if url.contains('?') { '&' } else { '?' };
    format!("{url}{separator}options=-c%20role%3Dnisse_ui")
}

#[test]
fn without_a_connection_the_app_says_so() {
    let app = start(None, "Add the store's connection in Settings.");
    assert!(!app.shows("Kill switch: off"));
}

#[test]
fn the_status_screen_shows_what_the_store_says() {
    let rt = runtime();
    let db = store_with_a_live_engine(&rt);
    let app = start(Some(&as_ui_role(&db.url)), "Kill switch: off");
    assert!(app.shows("Drawdown mode: normal"));
    assert!(app.shows("Rebalance owed: no"));
    assert!(!app.shows("Heartbeat: never"));
    drop(app);
    rt.block_on(db.drop_db()).unwrap();
}

#[test]
fn an_unreachable_store_is_cannot_confirm_never_a_state() {
    // Nothing listens on port 1.
    let app = start(
        Some("postgres://nobody@127.0.0.1:1/none"),
        "Cannot confirm the engine's state.",
    );
    assert!(!app.shows("Kill switch: off"));
    assert!(!app.shows("Kill switch: ENGAGED"));
}

#[test]
fn the_kill_switch_is_engaged_from_the_app_and_read_back() {
    let rt = runtime();
    let db = store_with_a_live_engine(&rt);
    let mut app = start(Some(&as_ui_role(&db.url)), "Kill switch: off");

    app.press("Kill").unwrap();
    app.wait_for("Engage kill switch").unwrap();
    app.type_text("Your name", "ana").unwrap();
    app.press("Engage kill switch").unwrap();
    app.wait_for("Engage the kill switch as ana?").unwrap();
    app.press("Yes, engage now").unwrap();
    app.wait_for("Kill switch ENGAGED").unwrap();

    // The store agrees, and the row says who and from where.
    let client = rt.block_on(db.client()).unwrap();
    let status = rt
        .block_on(nisse_ui_data::status(&client, "engine"))
        .unwrap();
    assert!(status.kill_switch);
    let row = rt
        .block_on(client.query_one(
            "SELECT detail->>'by', detail->>'via' FROM risk_event \
             WHERE kind = 'kill_switch' ORDER BY id DESC LIMIT 1",
            &[],
        ))
        .unwrap();
    assert_eq!(row.get::<_, String>(0), "ana");
    assert_eq!(row.get::<_, String>(1), "ui");

    // And the Status screen shows it after reading it afresh.
    app.press("Status").unwrap();
    app.wait_for("Kill switch: ENGAGED").unwrap();

    drop(app);
    drop(client);
    rt.block_on(db.drop_db()).unwrap();
}

#[test]
fn a_kill_that_cannot_reach_the_store_is_not_confirmed() {
    let mut app = start(
        Some("postgres://nobody@127.0.0.1:1/none"),
        "Cannot confirm the engine's state.",
    );
    app.press("Kill").unwrap();
    app.wait_for("Engage kill switch").unwrap();
    app.type_text("Your name", "ana").unwrap();
    app.press("Engage kill switch").unwrap();
    app.press("Yes, engage now").unwrap();
    app.wait_for("NOT CONFIRMED").unwrap();
    assert!(!app.shows("Kill switch ENGAGED"));
}

#[test]
fn positions_and_gates_show_what_is_stored() {
    let rt = runtime();
    let db = store_with_a_live_engine(&rt);
    let client = rt.block_on(db.client()).unwrap();
    let position = |symbol: &str, qty: &str, cost: &str, secs: u64| PositionEvent {
        symbol: Symbol::new_checked(symbol).unwrap(),
        qty_after: qty.parse().unwrap(),
        avg_cost: cost.parse().unwrap(),
        ts: UnixNanos::from(secs * 1_000_000_000),
    };
    rt.block_on(nisse_store::tables::position_event::insert_many(
        &client,
        &[
            position("SPY", "3", "500.25", 1_800_000_000),
            position("GLD", "10", "200", 1_800_000_000),
            // GLD is later closed: its newest row is flat, so it is not listed.
            position("GLD", "0", "0", 1_800_000_100),
        ],
    ))
    .unwrap();

    let mut app = start(Some(&as_ui_role(&db.url)), "Kill switch: off");
    app.press("Positions").unwrap();
    app.wait_for("SPY  3 @ 500.25").unwrap();
    assert!(!app.shows("GLD  10 @ 200"));
    // No order is open in this store (the two lists load separately).
    app.wait_for("OPEN ORDERS, AS THE ENGINE LAST RECORDED THEM")
        .unwrap();

    app.press("Gates").unwrap();
    app.wait_for("None yet.").unwrap();

    drop(app);
    drop(client);
    rt.block_on(db.drop_db()).unwrap();
}

#[test]
fn settings_saves_and_forgets_the_connection() {
    let (mut app, storage) = start_with_storage(None, "Add the store's connection in Settings.");
    app.press("Settings").unwrap();
    app.wait_for("No connection is saved.").unwrap();
    app.type_text(
        "text:postgres://user:password@host/db",
        "postgres://phone@10.0.0.2/nisse",
    )
    .unwrap();
    app.press("Save").unwrap();
    app.wait_for("A connection is saved.").unwrap();
    assert_eq!(
        storage.entries().get(URL_KEY).map(String::as_str),
        Some("postgres://phone@10.0.0.2/nisse")
    );

    app.press("Forget the connection").unwrap();
    app.wait_for("No connection is saved.").unwrap();
    assert!(!storage.entries().contains_key(URL_KEY));
}

#[test]
fn the_kill_button_needs_a_name_and_a_second_press() {
    let rt = runtime();
    let db = store_with_a_live_engine(&rt);
    let mut app = start(Some(&as_ui_role(&db.url)), "Kill switch: off");
    app.press("Kill").unwrap();
    app.wait_for("Engage kill switch").unwrap();

    // With no name the button does nothing.
    let _ = app.press("Engage kill switch");
    assert!(!app.shows("Yes, engage now"));

    // With a name, one press only asks; backing out leaves the switch alone.
    app.type_text("Your name", "ana").unwrap();
    app.press("Engage kill switch").unwrap();
    app.wait_for("Yes, engage now").unwrap();
    app.press("Cancel").unwrap();
    app.wait_for("Engage kill switch").unwrap();

    let client = rt.block_on(db.client()).unwrap();
    let rows: i64 = rt
        .block_on(client.query_one(
            "SELECT count(*) FROM risk_event WHERE kind = 'kill_switch'",
            &[],
        ))
        .unwrap()
        .get(0);
    assert_eq!(rows, 0);

    drop(app);
    drop(client);
    rt.block_on(db.drop_db()).unwrap();
}

#[test]
fn off_is_not_shown_as_good_news_when_the_engine_has_no_heartbeat() {
    let rt = runtime();
    let db = rt.block_on(ScratchDb::create());
    let app = start(
        Some(&as_ui_role(&db.url)),
        "Kill switch: off (engine not confirmed alive)",
    );
    assert!(app.shows("Heartbeat: never"));
    assert!(!app.shows("Kill switch: off"));
    drop(app);
    rt.block_on(db.drop_db()).unwrap();
}

#[test]
fn a_heartbeat_in_the_future_is_cannot_confirm() {
    // Some clock is wrong: the engine's liveness cannot be judged, so no state is shown.
    let rt = runtime();
    let db = rt.block_on(ScratchDb::create());
    let client = rt.block_on(db.client()).unwrap();
    rt.block_on(client.execute(
        "INSERT INTO heartbeat (source, event_ts) \
         VALUES ('engine', clock_timestamp() + interval '20 minutes')",
        &[],
    ))
    .unwrap();
    let app = start(
        Some(&as_ui_role(&db.url)),
        "Cannot confirm the engine's state.",
    );
    assert!(!app.shows("Kill switch: off"));
    drop(app);
    drop(client);
    rt.block_on(db.drop_db()).unwrap();
}

#[test]
fn a_store_that_goes_away_after_a_good_read_stops_being_shown() {
    let rt = runtime();
    let db = store_with_a_live_engine(&rt);
    let mut app = start(Some(&as_ui_role(&db.url)), "Kill switch: off");
    // The store disappears. The next refresh fails, and the old "off" must not stay up.
    rt.block_on(db.drop_db()).unwrap();
    app.press("Refresh").unwrap();
    app.wait_for("Cannot confirm the engine's state.").unwrap();
    assert!(!app.shows("Kill switch: off"));
}

#[test]
fn the_activity_screen_lists_the_engine_steps() {
    let rt = runtime();
    let db = store_with_a_live_engine(&rt);
    let client = rt.block_on(db.client()).unwrap();
    rt.block_on(client.execute(
        "INSERT INTO engine_step (run_key, step, status, detail, event_ts) \
         VALUES ('kill', 'cancel', 'done', '{\"open_orders\": 3}', clock_timestamp())",
        &[],
    ))
    .unwrap();
    let mut app = start(Some(&as_ui_role(&db.url)), "Kill switch: off");
    app.press("Activity").unwrap();
    app.wait_for("kill  cancel: done, just now").unwrap();
    drop(app);
    drop(client);
    rt.block_on(db.drop_db()).unwrap();
}

#[test]
fn an_old_cancel_row_is_shown_with_its_age() {
    let rt = runtime();
    let db = store_with_a_live_engine(&rt);
    let client = rt.block_on(db.client()).unwrap();
    rt.block_on(client.execute(
        "INSERT INTO engine_step (run_key, step, status, detail, event_ts) \
         VALUES ('kill', 'cancel', 'done', '{}', clock_timestamp() - interval '2 days 1 hour')",
        &[],
    ))
    .unwrap();
    let mut app = start(Some(&as_ui_role(&db.url)), "Kill switch: off");
    app.press("Activity").unwrap();
    app.wait_for("kill  cancel: done, 2 d ago").unwrap();
    assert!(!app.shows("kill  cancel: done, just now"));
    drop(app);
    drop(client);
    rt.block_on(db.drop_db()).unwrap();
}

#[test]
fn a_kill_without_a_running_watcher_says_orders_are_not_being_cancelled() {
    // The engine is alive but the watcher has never run (its cron line is missing).
    let rt = runtime();
    let db = rt.block_on(ScratchDb::create());
    let client = rt.block_on(db.client()).unwrap();
    rt.block_on(client.execute(
        "INSERT INTO heartbeat (source, event_ts) \
         VALUES ('engine', clock_timestamp() - interval '5 seconds')",
        &[],
    ))
    .unwrap();
    let mut app = start(Some(&as_ui_role(&db.url)), "Kill watcher: never ran");

    app.press("Kill").unwrap();
    app.wait_for("Engage kill switch").unwrap();
    app.type_text("Your name", "ana").unwrap();
    app.press("Engage kill switch").unwrap();
    app.press("Yes, engage now").unwrap();
    app.wait_for("Open orders are NOT being cancelled").unwrap();
    // The switch itself is engaged and says so.
    assert!(app.shows("Kill switch ENGAGED"));

    drop(app);
    drop(client);
    rt.block_on(db.drop_db()).unwrap();
}

#[test]
fn the_heartbeat_source_is_the_engine_configs() {
    // The module reads the heartbeat of source "engine"; the engine writes under the name in
    // its config. If the config changes, this fails and the module must change with it.
    let config: serde_json::Value =
        serde_json::from_str(include_str!("../../../../config/engine/paper.json")).unwrap();
    assert_eq!(config["source"], "engine");
    assert!(include_str!("../src/lib.rs").contains("const HEARTBEAT_SOURCE: &str = \"engine\";"));
}

#[test]
fn a_read_that_is_not_refreshed_stops_being_shown() {
    let rt = runtime();
    let db = store_with_a_live_engine(&rt);
    let mut app = start_with_manual_clock(&as_ui_role(&db.url), "Kill switch: off");

    // The store stops answering (a lock held on the table the status read needs), so the
    // refresh neither succeeds nor fails for a while. Eighty seconds pass on the device.
    let mut blocker = rt.block_on(db.client()).unwrap();
    let held = rt.block_on(blocker.transaction()).unwrap();
    rt.block_on(held.execute("LOCK TABLE risk_event IN ACCESS EXCLUSIVE MODE", &[]))
        .unwrap();
    app.fast_forward(std::time::Duration::from_secs(80))
        .unwrap();

    // The old "off" is taken down because of its age alone.
    app.wait_for("The last read is 80 s old and has not been refreshed.")
        .unwrap();
    assert!(!app.shows("Kill switch: off"));

    drop(app);
    rt.block_on(held.rollback()).unwrap();
    drop(blocker);
    rt.block_on(db.drop_db()).unwrap();
}

#[test]
fn the_kill_result_is_taken_down_after_a_while() {
    let rt = runtime();
    let db = store_with_a_live_engine(&rt);
    let mut app = start_with_manual_clock(&as_ui_role(&db.url), "Kill switch: off");
    app.press("Kill").unwrap();
    app.wait_for("Engage kill switch").unwrap();
    app.type_text("Your name", "ana").unwrap();
    app.press("Engage kill switch").unwrap();
    app.press("Yes, engage now").unwrap();
    app.wait_for("Kill switch ENGAGED").unwrap();
    // The watcher is alive, but a cancel is not claimed: only that it is not yet confirmed.
    assert!(app.shows("Cancel of open orders: not yet confirmed"));

    app.fast_forward(std::time::Duration::from_secs(80))
        .unwrap();
    app.wait_for("The earlier result is no longer current. Read Status for the state now.")
        .unwrap();
    assert!(!app.shows("Kill switch ENGAGED"));

    drop(app);
    rt.block_on(db.drop_db()).unwrap();
}

#[test]
fn a_kill_with_a_late_watcher_says_orders_are_not_being_cancelled() {
    // The watcher last ran ten minutes ago: it is not relied on.
    let rt = runtime();
    let db = rt.block_on(ScratchDb::create());
    let client = rt.block_on(db.client()).unwrap();
    rt.block_on(client.execute(
        "INSERT INTO heartbeat (source, event_ts) VALUES \
         ('engine', clock_timestamp() - interval '5 seconds'), \
         ('kill-watch', clock_timestamp() - interval '10 minutes')",
        &[],
    ))
    .unwrap();
    let mut app = start(Some(&as_ui_role(&db.url)), "Kill switch: off");
    app.press("Kill").unwrap();
    app.wait_for("Engage kill switch").unwrap();
    app.type_text("Your name", "ana").unwrap();
    app.press("Engage kill switch").unwrap();
    app.press("Yes, engage now").unwrap();
    app.wait_for("Open orders are NOT being cancelled").unwrap();

    // Status says the same, with the way out.
    app.press("Status").unwrap();
    app.wait_for("THE KILL SWITCH IS ENGAGED BUT NOT ENFORCED")
        .unwrap();
    assert!(app.shows("Open orders: no cancel recorded, and the kill watcher is not running"));

    drop(app);
    drop(client);
    rt.block_on(db.drop_db()).unwrap();
}

/// A store with a live engine and watcher, the switch engaged a minute ago, and one
/// `kill` / `cancel` step of `status` recorded just now.
fn store_with_a_kill_and_its_cancel(rt: &tokio::runtime::Runtime, status: &str) -> ScratchDb {
    let db = store_with_a_live_engine(rt);
    let client = rt.block_on(db.client()).unwrap();
    rt.block_on(client.execute(
        "INSERT INTO risk_event (kind, detail, event_ts) VALUES \
         ('kill_switch', '{\"engaged\": true, \"by\": \"ana\"}', \
          clock_timestamp() - interval '1 minute')",
        &[],
    ))
    .unwrap();
    rt.block_on(client.execute(
        "INSERT INTO engine_step (run_key, step, status, detail, event_ts) \
         VALUES ('kill', 'cancel', $1, '{}', clock_timestamp())",
        &[&status],
    ))
    .unwrap();
    db
}

#[test]
fn status_says_cancelled_only_for_a_confirmed_cancel() {
    let rt = runtime();
    let db = store_with_a_kill_and_its_cancel(&rt, "done");
    let app = start(
        Some(&as_ui_role(&db.url)),
        "Open orders: cancel confirmed by the broker's own list (just now)",
    );
    assert!(app.shows("Kill switch: ENGAGED"));
    assert!(!app.shows("THE KILL SWITCH IS ENGAGED BUT NOT ENFORCED"));
    drop(app);
    rt.block_on(db.drop_db()).unwrap();

    let db = store_with_a_kill_and_its_cancel(&rt, "started");
    let app = start(
        Some(&as_ui_role(&db.url)),
        "Open orders: cancel sent, NOT yet confirmed (just now)",
    );
    assert!(!app.shows("Open orders: cancel confirmed by the broker's own list (just now)"));
    drop(app);
    rt.block_on(db.drop_db()).unwrap();
}

#[test]
fn status_shows_a_failed_cancel_and_the_way_out() {
    let rt = runtime();
    let db = store_with_a_kill_and_its_cancel(&rt, "failed");
    let app = start(
        Some(&as_ui_role(&db.url)),
        "Open orders: the cancel FAILED (just now)",
    );
    assert!(app.shows("THE KILL SWITCH IS ENGAGED BUT NOT ENFORCED"));
    assert!(app.shows("Open orders are NOT being cancelled"));
    drop(app);
    rt.block_on(db.drop_db()).unwrap();
}
