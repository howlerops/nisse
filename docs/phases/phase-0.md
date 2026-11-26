# Phase 0: Scaffold

Issue: #2. Exit criterion: CI green; migrations apply cleanly.

Phase 0 has no engine code, so there is no hot path and no benchmark yet. The first
`criterion` benches arrive with the first per-bar code (Phase 2/3).

## Decisions taken in this phase

| Decision | Choice | Why |
|---|---|---|
| NautilusTrader pin | `=0.68.0` (`nautilus-core`, `nautilus-model`), released 2026-11-10 | Latest stable on crates.io. Licence is LGPL-3.0-only (was `-or-later` up to at least 0.60); the open question in #12 stays open. |
| Rust toolchain | 1.99.1, in `rust-toolchain.toml` | `nautilus-* 0.68.0` declares it as its minimum. Bump with the Nautilus pin. |
| Other Nautilus crates | Not added yet | `nautilus-common`, `-backtest`, `-live`, `-execution` are added at the same exact version by the phase that first uses them (1 to 3). |
| Migrations | Plain SQL in `migrations/`, applied by `scripts/migrate.sh` with `psql` | No new dependency in Phase 0. The Rust Postgres client is chosen in Phase 2, when the writer exists. |
| Append-only | Enforced in the database by a trigger on every table | A rule that is only a convention gets broken; this also covers ad-hoc `psql` sessions. |
| Enum columns | `TEXT` with a `CHECK`, mirrored by `strum` enums in `nisse-core` | Readable rows; adding a value is one migration plus one variant. |
| Number columns | Domains `finite_numeric` (money, quantity) and `finite_double` (ratios, statistics) | `NUMERIC` is exact and maps to Nautilus fixed-point `Price` / `Quantity`; never `f64` for money. The domains reject NaN and Infinity, which Postgres otherwise accepts and sorts above every number. A contract test rejects bare `NUMERIC` / float columns. |

Differences from the PLAN.md table list (PLAN.md is updated to match):

- `ts` is named `event_ts` everywhere.
- `order_event` gains `client_order_id`: the write-ahead intent row exists before the broker
  has assigned `broker_order_id`, which is therefore nullable.
- `fill.order_id` is `client_order_id` (an order is many `order_event` rows, so there is no
  single row to point at).
- `fill` gains `broker_fill_id`, unique: cumulative quantity is the sum of an order's fills,
  so a fill delivered twice must be rejected, not counted twice.
- `risk_check.limit` / `actual` are `limit_value` / `actual_value` (`LIMIT` is reserved in SQL).

Left for the phase that writes the rows:

- Phase 1: the `order_event.state` and `type` value lists (a `CHECK`, mirroring the adapter's
  state machine).
- Phase 2: a writer role with `INSERT` and `SELECT` only (no `ingest_ts` column grant), so the
  engine cannot disable the append-only trigger or backdate `ingest_ts`; and how a correcting
  row names the row it corrects.
- Before Phase 3: how a trial's result is attached to a row registered before the run. Rows
  cannot be updated and the trial count is the row count, so a second row would double-count.
  Recommended: register first with `result NULL`, and put results in a separate
  `trial_result` table keyed by `trial_registry.id` (an eleventh table; the owner decides).

## Interfaces

### Rust (`crates/core`, written by the lead)

```rust
pub enum Action { Enter, Exit, Hold, Skip }            // decision.action
pub enum TrialKind { Backtest, Shadow, Live }          // trial_registry.kind
pub enum SubjectType { Decision, StrategyVersion }     // eval_score.subject_type
// each: Display, FromStr, Into<&'static str>, VariantArray; snake_case strings

pub struct StrategyVersionId(pub i64);
pub struct DecisionId(pub i64);

pub struct Hash32([u8; 32]);                            // config_hash, feature_hash
impl Hash32 {
    pub const fn from_bytes(bytes: [u8; 32]) -> Self;
    pub const fn as_bytes(&self) -> &[u8; 32];
}
impl FromStr for Hash32 { type Err = CoreError; }       // 64 hex chars, either case
impl Display for Hash32 {}                              // 64 lowercase hex chars, no allocation

pub enum CoreError { InvalidHashLength(usize), InvalidHashChar(usize), UnknownVariant(strum::ParseError) }

pub mod nautilus { /* re-exports: UnixNanos, Price, Quantity, Money, InstrumentId, Symbol,
                      Venue, ClientOrderId, VenueOrderId, StrategyId, OrderSide, OrderStatus,
                      OrderType, TimeInForce */ }
```

Every other crate has one `thiserror` error enum (`EngineError`, `RiskError`, `AdapterError`,
`StoreError`, `EvalError`) and nothing else yet. No traits are defined in Phase 0: none has
two implementations or a test double yet.

### SQL

Shared, written by the lead: `migrations/0001_append_only.sql` (`nisse_make_append_only`),
`scripts/migrate.sh`, `scripts/test-migrations.sh`, `migrations/tests/support/helpers.sql`
(`pg_temp.assert_true`, `assert_raises`, `assert_append_only`) and the contract test
`migrations/tests/0001_append_only.sql`, which checks every table in the schema.

Every store table starts with `id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY`, has
`event_ts TIMESTAMPTZ NOT NULL` and `ingest_ts TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp()`
as its last two columns, and ends with `SELECT nisse_make_append_only('<table>');`. Columns
below are `NOT NULL` unless marked `NULL`. `hash` means `BYTEA` with
`CHECK (octet_length(col) = 32)`. `NUMERIC` and `DOUBLE PRECISION` below mean the
`finite_numeric` and `finite_double` domains. `fk(t)` means `BIGINT REFERENCES t (id)`.

| Migration | Table | Columns (besides `id`, `event_ts`, `ingest_ts`) | Constraints and indexes |
|---|---|---|---|
| 0002 | `strategy_version` | `git_sha TEXT`, `config_hash` hash, `params JSONB`, `parent_id` fk(strategy_version) NULL | `CHECK (git_sha ~ '^[0-9a-f]{40}$')`; `UNIQUE (git_sha, config_hash)` |
| 0002 | `trial_registry` | `strategy_version_id` fk(strategy_version), `kind TEXT`, `data_range TSTZRANGE`, `result JSONB NULL` | `CHECK (kind IN ('backtest','shadow','live'))`; `CHECK (NOT isempty(data_range))`; index `(strategy_version_id)` |
| 0003 | `decision` | `strategy_version_id` fk(strategy_version), `symbol TEXT`, `action TEXT`, `intended_qty NUMERIC`, `expected_edge_bps DOUBLE PRECISION NULL`, `expected_vol DOUBLE PRECISION NULL`, `features JSONB`, `feature_hash` hash, `data_snapshot_id TEXT` (non-empty), `rng_seed BIGINT` (the `u64` seed's bits), `reason_code TEXT` | `CHECK (action IN ('enter','exit','hold','skip'))`; `CHECK (symbol <> '')`; `CHECK (reason_code <> '')`; index `(strategy_version_id, event_ts)` |
| 0003 | `market_snapshot` | `symbol TEXT`, `bid NUMERIC NULL`, `ask NUMERIC NULL`, `mid NUMERIC NULL`, `spread_bps DOUBLE PRECISION NULL`, `adv NUMERIC NULL`, `vol DOUBLE PRECISION NULL`, `regime_tag TEXT NULL` | `CHECK (symbol <> '')`; `CHECK (bid <= ask)`; index `(symbol, event_ts)` |
| 0003 | `risk_check` | `decision_id` fk(decision), `rule TEXT`, `limit_value NUMERIC`, `actual_value NUMERIC`, `passed BOOLEAN` | `CHECK (rule <> '')`; index `(decision_id)` |
| 0004 | `order_event` | `decision_id` fk(decision), `client_order_id TEXT`, `broker_order_id TEXT NULL`, `state TEXT`, `type TEXT`, `limit_px NUMERIC NULL`, `qty NUMERIC` | `CHECK (client_order_id <> '')`; `CHECK (state <> '')`; `CHECK (type <> '')`; `CHECK (qty > 0)`; `CHECK (limit_px > 0)`; index `(client_order_id, event_ts)`; index `(decision_id)` |
| 0004 | `fill` | `client_order_id TEXT`, `broker_fill_id TEXT` (non-empty, `UNIQUE`), `px NUMERIC`, `qty NUMERIC`, `fee NUMERIC` | `CHECK (client_order_id <> '')`; `CHECK (px > 0)`; `CHECK (qty > 0)`; index `(client_order_id, event_ts)` |
| 0004 | `position_event` | `symbol TEXT`, `qty_after NUMERIC`, `avg_cost NUMERIC` | `CHECK (symbol <> '')`; `CHECK (avg_cost >= 0)`; index `(symbol, event_ts)` |
| 0005 | `outcome_window` | `decision_id` fk(decision), `horizon INTERVAL`, `fwd_ret DOUBLE PRECISION`, `bench_ret DOUBLE PRECISION`, `mfe DOUBLE PRECISION`, `mae DOUBLE PRECISION` | `CHECK (horizon > INTERVAL '0')`; index `(decision_id, horizon)` |
| 0005 | `eval_score` | `subject_id BIGINT`, `subject_type TEXT`, `metric TEXT`, `value DOUBLE PRECISION`, `eval_version TEXT` | `CHECK (subject_type IN ('decision','strategy_version'))`; `CHECK (metric <> '')`; `CHECK (eval_version <> '')`; index `(subject_type, subject_id, metric)` |

`fee` may be negative (rebates) and `qty_after` may be negative (short), so neither has a
sign check. `fill.qty` is the quantity of that one fill; cumulative quantity is derived by
the reader. A `CHECK` on a nullable column passes when the value is `NULL`.

## Tasks

| # | Task | Files | Owner | Depends on |
|---|---|---|---|---|
| F | Workspace, lints, toolchain pin, `crates/core`, crate stubs, 0001 guard, scripts, SQL test helpers | (foundation) | lead | none |
| T1 | `strategy_version`, `trial_registry` | `migrations/0002_strategy_trial.sql`, `migrations/tests/0002_strategy_trial.sql` | worker | F |
| T2 | `decision`, `market_snapshot`, `risk_check` | `migrations/0003_decision.sql`, `migrations/tests/0003_decision.sql` | worker | T1 |
| T3 | `order_event`, `fill`, `position_event` | `migrations/0004_orders.sql`, `migrations/tests/0004_orders.sql` | worker | T2 |
| T4 | `outcome_window`, `eval_score` | `migrations/0005_evals.sql`, `migrations/tests/0005_evals.sql` | worker | T2 |
| T5 | CI workflow | `.github/workflows/ci.yml` | worker | F |
| T6 | `Hash32` parse and format, enum string tests | `crates/core/src/hash.rs`, `crates/core/tests/store_enums.rs` | worker | F |
| L | Table-list test, PLAN.md, README, PR | `migrations/tests/0006_store_tables.sql` and docs | lead | all |

Rounds: {T1, T5, T6} in parallel, then T2, then {T3, T4} in parallel. No two tasks share a file.

## Not done by this phase's PR

- Branch protection and required CODEOWNERS review on `main` are repository settings; the
  owner turns them on.
