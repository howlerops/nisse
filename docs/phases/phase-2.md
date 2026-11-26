# Phase 2: Decision store

Issue: #4. Exit criterion: nightly replay test reproduces every decision.

## Decisions taken in this phase

| Decision | Choice | Why |
|---|---|---|
| Postgres client | `tokio-postgres`, no TLS | Small and direct; the engine and its database share a host. Do not point it at a remote database. |
| Two write paths | `Store` (awaited) for decisions and order intents; `StoreWriter` (bounded queue → writer task → one transaction per batch) for everything else | A decision's id is needed before an order can refer to it, and the intent must be on disk before the order is sent. Nothing else may make the engine wait on the database. |
| Store failure | The writer stops on the first database error and every later `emit` fails | Fail closed: an engine that cannot record must stop trading, not carry on unrecorded. |
| Fill + order row | Written in one transaction (`StoreRecord::Order`) | Otherwise a crash between the two would make a restart count the fill again. |
| Timestamps | Nanoseconds in the engine, truncated to microseconds in Postgres | `TIMESTAMPTZ` resolution. Nothing is keyed on exact time. |
| Hashes | SHA-256 over canonical JSON (sorted keys, no whitespace); feature values are decimal strings | Stable bytes, no floats. New dependency: `sha2`. |
| Strategy interface | `Strategy::decide(&Features, seed) -> Verdict`, pure | Replay needs the decision to depend on nothing but stored inputs. The current position is a feature. |
| Replay | Per build: a binary replays the decisions made at its own git SHA; `scripts/replay.sh` runs it once per SHA | A decision can only be reproduced by the code that made it. |
| Roles | `nisse_writer` (INSERT and SELECT, no `id` / `ingest_ts` columns) and `nisse_reader` (SELECT), both NOLOGIN group roles | The engine cannot disable the append-only trigger or backdate `ingest_ts`. Login users are created at deploy time (Phase 6). |
| Logging | `tracing` | Alerts are `tracing::error!` events for now; Phase 6 routes them. |
| Store tests | Real Postgres via `TEST_DATABASE_URL`; they fail when it is unset | A store test that silently skips proves nothing. `scripts/dev-postgres.sh` starts a local server. |

## Hot path

Per decision: `decide` (pure) and one awaited `decision` insert. Per order: one awaited intent
insert. Per broker message: `record_stream_update` (tracker, position book, one queue send).
Volumes are tiny for a monthly strategy, so there is still no criterion bench in this phase;
the rules that apply are: no `f64` for money, no lock across `.await`, bounded channels with
back-pressure tested, batched multi-row inserts, and SQL only in `crates/store/src/tables/`.

## Interfaces (written by the lead)

- `crates/core/src/canonical.rs`: `Features`, `canonical_json`, `config_hash`,
  `features_to_json`, `features_from_json`, `feature_hash`.
- `crates/core/src/record.rs`: `StrategyVersion`, `Trial`, `Decision`, `MarketSnapshot`,
  `PositionEvent`, `RiskCheck`, `OrderEvent`, `RestoredOrder`, `EvalScore`, `StoreRecord`,
  traits `DecisionLog` and `RecordSink`.
- `crates/store`: `connect`, `Store` (implements `DecisionLog` and `IntentLog`),
  `StoreWriter` (implements `RecordSink`), `tables::*`, `testing::ScratchDb`
  (feature `test-support`), shared conversions in `db.rs` (`to_ts`, `from_ts`, `to_quantity`,
  `to_price`, `parse_enum`).
- `crates/engine`: `Strategy`, `Verdict`, `decide`, `decide_and_record`, `replay`,
  `ReplayOutcome`, `PositionBook`, `OrderBookState`, `record_stream_update`,
  `reconcile_and_record`, `OrderTracker::restore_all`, `GIT_SHA`.
- `crates/eval/src/replay.rs`: `replay_decisions`, `ReplayReport`.

## SQL conventions for `crates/store/src/tables/`

- Only `INSERT` and `SELECT`. Never `UPDATE`, `DELETE`, `TRUNCATE`, and no `ON CONFLICT ... DO
  UPDATE`.
- Never set `id` or `ingest_ts`; the database fills them.
- Domain columns (`finite_numeric`, `finite_double`) are bound and read through the base type
  with a cast: `$1::numeric`, `$2::float8`, `SELECT px::numeric`. Arrays: `$1::numeric[]`.
- Types: `rust_decimal::Decimal` ↔ `numeric`; `f64` ↔ `float8`; `String`/`&str` ↔ `text`;
  `chrono::DateTime<Utc>` ↔ `timestamptz` (via `db::to_ts` / `db::from_ts`);
  `serde_json::Value` ↔ `jsonb`; `&[u8]` ↔ `bytea` (`Hash32::as_bytes()`); `i64` ↔ `bigint`.
- `Price` / `Quantity` are bound as `.as_decimal()` and read back with `db::to_price` /
  `db::to_quantity`. Ids (`Symbol`, `ClientOrderId`, ...) are bound as `.as_str()` and rebuilt
  with the `new_checked` constructors (an invalid stored id is `StoreError::Data`).
- Enum columns: `OrderState`, `Action`, `TrialKind`, `SubjectType` are bound as
  `<&'static str>::from(value)` (snake_case) and read with `db::parse_enum`. Nautilus
  `OrderSide`, `OrderType`, `TimeInForce` are bound as `value.to_string()` (`BUY`, `LIMIT`,
  `DAY`) and read with `db::parse_enum` too.
- `rng_seed`: `u64` stored as the `i64` with the same bits (`seed as i64` via
  `i64::from_ne_bytes(seed.to_ne_bytes())`, and back).
- Multi-row inserts: one statement,
  `INSERT INTO t (a, b) SELECT * FROM UNNEST($1::text[], $2::numeric[])`, one `Vec` per
  column, each reserved to `rows.len()`.
- Tests are integration tests in `crates/store/tests/*.rs` using
  `nisse_store::testing::ScratchDb::create().await` (needs `TEST_DATABASE_URL`; run
  `eval "$(scripts/dev-postgres.sh)"`), and end with `db.drop_db().await`.

## Tasks

| # | Task | Files | Round |
|---|---|---|---|
| F | Deps, core records and traits, store connection/test support/stubs, engine and eval stubs, CI database | (foundation) | lead |
| C1 | Canonical JSON and hashes | `crates/core/src/canonical.rs` | 1 |
| M1 | Roles migration | `migrations/0007_roles.sql`, `migrations/tests/0008_roles.sql` | 1 |
| L | Review fixes: `submit_and_record`, `load_live`, migration 0008, hash check | (lead) | after review |
| S1 | `strategy_version`, `trial_registry`, `decision` | those three table modules, `crates/store/tests/decisions.rs` | 1 |
| S2 | `order_event`, `fill` | those two table modules, `crates/store/tests/orders.rs` | 1 |
| S3a | `market_snapshot`, `position_event` | those two table modules, `crates/store/tests/snapshots.rs` | 1 |
| S3b | `risk_check`, `eval_score` | those two table modules, `crates/store/tests/scores.rs` | 1 |
| E1 | Decide, record, replay | `crates/engine/src/decide.rs` | 1 |
| E2 | Position book | `crates/engine/src/positions.rs` | 1 |
| S4 | Store writer | `crates/store/src/writer.rs`, `crates/store/tests/writer.rs` | 2 (needs S2, S3a, S3b) |
| E3 | Order service | `crates/engine/src/order_service.rs`, `crates/engine/tests/order_service.rs`, `crates/engine/tests/support/mod.rs` (add a `MemorySink`) | 2 (needs E2) |
| V1 | Replay job | `crates/eval/src/replay.rs`, `crates/eval/src/bin/nisse-replay.rs`, `scripts/replay.sh` | 3 (needs S1, S3b, E1) |

### Acceptance tests

**C1.** `canonical_json`: nested objects with keys given out of order come out sorted at every
level; arrays keep order; no whitespace; strings with quotes, backslashes and non-ASCII are
escaped exactly as `serde_json::to_string` escapes them; two `Value`s that differ only in key
order give identical text. `config_hash`: equal for reordered keys, different for a changed
value; the hash of `{"a":1}` equals SHA-256 of the bytes `{"a":1}` (assert the hex:
`015abd7f5cc57a2dd94b7590f04ad8084273905ee33ec5cebeae62276a97f862`). `features_to_json`: values
are JSON strings; `1.50` and `1.5` both become `"1.5"`; `-0` becomes `"0"`.
`features_from_json`: round-trips; rejects a non-object, a number value, a non-decimal string
(`"abc"`, `"1e5"`, `""`). `feature_hash`: equal for `1.50` vs `1.5`; differs when a value or a
name changes; independent of insertion order; the empty map hashes to SHA-256 of `{}`
(`44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a`).

**M1.** `0007_roles.sql` creates NOLOGIN roles `nisse_writer` and `nisse_reader` if they do
not exist (roles are cluster-wide, so use a `DO` block checking `pg_roles`); grants `USAGE` on
schema `public`; `nisse_reader`: `SELECT` on every store table; `nisse_writer`: `SELECT` on
every store table and column-level `INSERT` on every column except `id` and `ingest_ts`, plus
`USAGE` on the identity sequences; neither gets `UPDATE`, `DELETE`, `TRUNCATE`, `REFERENCES`
or `TRIGGER`, and neither may touch `schema_migration` beyond `SELECT`. Write it as a `DO` block that loops
over the store tables and builds each column list from `information_schema.columns`, so the
ten tables are not spelled out ten times. Test (`SET LOCAL ROLE nisse_writer` inside the
test transaction, `RESET ROLE` to check as owner): a full valid insert chain
(strategy_version → decision → order_event → fill) succeeds as the writer; as the writer,
each of these fails with SQLSTATE 42501: `UPDATE`, `DELETE`, `TRUNCATE` on a store table;
`ALTER TABLE fill DISABLE TRIGGER append_only`; an `INSERT` that supplies `ingest_ts`; an
`INSERT` into `schema_migration`; `CREATE TABLE`. As the reader: `SELECT` works, `INSERT`
fails with 42501. Rows inserted by the writer have `ingest_ts` set by the database.

**S1.** `strategy_version::ensure` returns the same id when called twice with the same
`(git_sha, config_hash)` and does not change the first row (a second call with different
`params` still reads back the first `params`); different hash → different id; `parent_id`
round-trips; `get` of a missing id is `None`; a bad `git_sha` is `StoreError::Db`.
`trial_registry::insert` then `lifetime_count` counts every row of every kind; the stored
range is `[start, end)`; `result` `None` and `Some` both work; an empty range (`start ==
end`) is `StoreError::Db`. `decision::insert` + `list_after`: every field of `Decision`
round-trips exactly (features with several decimals, both `Some` and `None` statistics, a
seed above `i64::MAX`, timestamps to the microsecond), `feature_hash` bytes are preserved,
paging with `after_id` and `limit` returns rows in id order without gaps or repeats, and the
joined `StrategyVersion` equals what was ensured; an unknown `strategy_version_id` is
`StoreError::Db`.

**S2.** `order_event::insert` of `OrderEvent::intent(..)` then later rows; `load_open`
returns one `RestoredOrder` per order whose latest row is non-terminal, with the intent
rebuilt from the intent row (all fields), state / venue id / cumulative quantity / last
timestamp from the latest row, and `avg_fill_px` as the quantity-weighted average of its
fills (two fills 2 @ 10 and 3 @ 11 → 10.6; no fills → `None`); orders whose latest row is
`filled`, `canceled`, `rejected`, `replaced` or `expired` are absent; results are in client
order id order; latest means highest `event_ts` then highest `id` (test two rows with equal
`event_ts`). `fill::insert` returns `true` then `false` for the same `trade_id`, and the
table holds one row; price and quantity round-trip exactly; a market order (`limit_px` None)
and a limit order both insert. An `order_event` with an unknown `decision_id` is
`StoreError::Db`.

**S3a / S3b.** For each table: `insert_many` of three rows sends one statement and all three
read back equal (query the table in the test and compare field by field, including `None`
columns and exact decimals); an empty slice is `Ok` and inserts nothing; a row violating a
constraint (empty symbol / empty rule / unknown `decision_id` / NaN `value`) makes the whole
call fail with `StoreError::Db` and inserts none of the batch; `ingest_ts` is set by the
database; 1,000 rows insert in one call.

**E1.** With small test strategies defined in the test module (one that thresholds a
feature, one that uses the seed, one that returns `NaN` statistics): `decide` copies every
context field into the `Decision`, calls `decide` on the strategy exactly once (count calls
with an `AtomicUsize`), sets `feature_hash` to `nisse_core::feature_hash(&features)`, and
maps the verdict's fields. `decide_and_record` records exactly that decision in a memory
`DecisionLog` and returns its id; holds and skips are recorded too; a failing log gives
`DecisionNotRecorded`. `replay`: an unmodified decision is `Match`, including one with `NaN`
statistics; tampered features (hash no longer matches) are `FeatureHashMismatch`; changing
the recorded action / quantity / reason / edge / vol gives `Mismatch` naming that field, and
when several differ the first in the documented order is reported; a strategy whose decision
depends on the seed mismatches when the stored seed is changed; replaying the same decision
1,000 times always matches.

**E2.** `apply_fill`: open long from flat; add to a long (weighted average: 10 @ 100 then 10
@ 110 → 20 @ 105); reduce a long (cost unchanged); close exactly (removed, event 0 / 0); cross
from long to short (remainder at the fill price); the same four cases mirrored for shorts;
fractional quantities stay exact (0.1 + 0.2 = 0.3); `get` of an unknown symbol is flat;
`quantities()` omits flat symbols and is in symbol order. `adopt`: same figures → `None`;
different quantity with a broker average → adopted; `None` average keeps the old cost when
the sign is unchanged and zeroes it when the sign flips; broker flat removes the position
and returns an event with 0 / 0; adopting flat when already flat → `None`.

**S4.** Against a scratch database: records of all four kinds emitted then `flush` are all in
their tables; an `Order` record with a fill writes both rows, and emitting the same fill
again (same `trade_id`) writes the order row but not a second fill; order within a table
follows emit order; back-pressure: with `capacity: 1`, 200 records emitted from one task all
arrive, in order, and `emit` never returns an error; a batch is one transaction: a record
that violates a constraint (unknown `decision_id`) makes the task end with `Err`, none of
that batch's rows are in the database, and every later `emit` and `flush` returns
`WriterStopped`; dropping every handle ends the task with `Ok(())` after the queue is
drained (records emitted just before the drop are in the database); `flush` on an idle
writer returns promptly.

**E3.** With `FakeBroker` and a new `MemorySink` (records every `StoreRecord` in order; can
be set to fail): a stream fill emits one `Order` record (row state and cumulative quantity
from the tracker, with the fill) followed by one `Position` record, and the position book
holds the quantity; a state change without a fill emits one `Order` record and no
`Position`; a duplicate stream message emits nothing; an update for an untracked order
returns `Ok(false)` and emits nothing; a sell fill reduces the position; a failing sink gives
`Store(..)`. `reconcile_and_record`: a clean pass emits nothing; a fill found by the pass
emits an `Order` record with the fill and does **not** change the position book from the
fill (the position comes from the broker: a `PositionMismatch` in the same pass adopts the
broker's quantity and average and emits one `Position` record); a never-arrived verdict
emits an `Order` record with state `rejected`; a revived order emits one with its new
state; a broker failure gives `Broker(..)` and emits nothing.

**V1.** Against a scratch database, with a test strategy and factory: decisions recorded
through `decide_and_record` + `Store` all replay as `matched`, and each gets an `eval_score`
row with value 1; a decision whose stored action is then contradicted (insert a decision
row whose action differs from what the strategy returns) is in `failures` with value 0;
decisions under another git SHA are counted in `other_builds` and get no score; a version
whose `params` do not hash to its `config_hash`, or that the factory rejects, is counted in
`unbuildable` with value 0; more than one page of decisions (use 1,200) is fully processed;
`after` skips earlier ids; the factory is called once per strategy version. `nisse-replay`
(binary): reads `DATABASE_URL`, replays for `nisse_engine::GIT_SHA`, prints the report, exits
0 when clean and 1 otherwise; with no strategies registered yet its factory returns
`UnknownStrategy` for every name. `scripts/replay.sh`: lists the distinct git SHAs in
`strategy_version`, and for each one that is not the current checkout's HEAD prints that it
needs a build of that SHA (building other SHAs automatically is Phase 6 deployment work),
then runs `cargo run -p nisse-eval --bin nisse-replay` for HEAD; exits non-zero if the
binary does.

## Rules added after the independent review

- **A failed lookup does not end a reconciliation pass.** It is a `SyncProblem` for that
  order; fills already applied in the pass are still reported and recorded.
- **Gateway outcomes are recorded.** `submit_and_record` writes the row for what the broker
  answered: an order that comes back already filled gets its fill and position recorded
  (the later stream message carries no new quantity), and a rejection gets a `rejected` row.
- **Restart loading uses insertion order.** `order_event::load_live` takes an order's latest
  row by `id`, not `event_ts`: event times come from two clocks. Rows written by
  reconciliation carry the order's own last-update time, not the engine clock.
- **Recently ended orders are restored too.** `load_live(client, Some(since))` also returns
  orders whose latest row is terminal but was stored since `since`, so a fill racing a
  cancel, or an order under a provisional never-arrived verdict, is still known after a
  restart. A restored `rejected` order with no broker id is treated as provisional.
- **One intent row per client order id** (migration 0008, a partial unique index).
- **`strategy_version::ensure` refuses a version whose `config_hash` is not the hash of its
  `params`.**
- **`scripts/replay.sh` exits 3 when another build's decisions were skipped.** "Replay
  passed" must not mean "nothing was replayed".
- **`nisse_engine::build_is_replayable()`** is false for a dirty tree or an unknown commit; a
  trading process must refuse to start then (enforced when the engine binary exists).

## Not done by this phase's PR

- Scheduling: nothing runs the replay job nightly yet (cron on the engine host is Phase 6).
- The replay job starts from the first decision every run and writes a new score row for
  each, so `eval_score` grows by the decision count per run. Acceptable at this volume; a
  stored cursor can come with Phase 5.
- One undecodable decision row stops the replay job (loudly) rather than being scored 0.
- `nisse-replay` installs no `tracing` subscriber; its alert is the printed report and the
  exit status until Phase 6 routes logs.
- Open orders and positions are read at two instants, so a fill landing between them can be
  counted on top of an adopted position until the next pass corrects it. A decision made in
  that window would see a wrong position feature; the engine loop (Phase 3/4) must reconcile
  before it decides.

- A running engine process: the pieces are wired into a binary in Phase 3 (strategy and
  backtest) and Phase 4 (risk gate). This phase delivers the store, the recording paths and
  the replay job.
- Login roles and the production connection string: Phase 6.
- `outcome_window` writes: Phase 5.
