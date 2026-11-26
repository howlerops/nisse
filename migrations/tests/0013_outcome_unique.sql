-- Uniqueness checks for outcome_window on (decision_id, horizon) (0012_outcome_unique.sql). Runs
-- inside the runner's transaction, which is rolled back. Only the owner runs here: no role is
-- switched, so these checks exercise the index and not the grants.

-- Parent rows, as owner: a strategy_version and two decisions with their own snapshot id.
--   enter is the decision under test; exit is a second decision for the same horizons.
INSERT INTO strategy_version (git_sha, config_hash, params, event_ts)
VALUES (repeat('a', 40), decode(repeat('ab', 32), 'hex'), '{}', '2026-01-02T00:00:00Z');

INSERT INTO decision (strategy_version_id, symbol, action, intended_qty, features, feature_hash,
                      data_snapshot_id, rng_seed, reason_code, event_ts)
VALUES ((SELECT id FROM strategy_version WHERE git_sha = repeat('a', 40)),
        'SPY', 'enter', 1, '{}', decode(repeat('ab', 32), 'hex'),
        'snap-outcome-unique', 42, 'trend_up', '2026-01-05T21:00:00Z'),
       ((SELECT id FROM strategy_version WHERE git_sha = repeat('a', 40)),
        'SPY', 'exit', 1, '{}', decode(repeat('ab', 32), 'hex'),
        'snap-outcome-unique', 42, 'trend_up', '2026-01-05T21:00:00Z');

-- 1. The first row for (enter, 5 days) is accepted.
INSERT INTO outcome_window (decision_id, horizon, fwd_ret, bench_ret, mfe, mae, event_ts)
VALUES ((SELECT id FROM decision WHERE data_snapshot_id = 'snap-outcome-unique' AND action = 'enter'),
        INTERVAL '5 days', 0.01, 0.004, 0.02, -0.01, '2026-01-12T21:00:00Z');

-- 2. A second row for the same decision and horizon is refused with 23505, even with other values.
SELECT pg_temp.assert_raises($q$
  INSERT INTO outcome_window (decision_id, horizon, fwd_ret, bench_ret, mfe, mae, event_ts)
  VALUES ((SELECT id FROM decision WHERE data_snapshot_id = 'snap-outcome-unique' AND action = 'enter'),
          INTERVAL '5 days', -0.5, 0.4, 0.2, -0.1, '2026-01-12T21:00:00Z')
$q$, '23505');

-- 3. The same decision with a different horizon is accepted.
INSERT INTO outcome_window (decision_id, horizon, fwd_ret, bench_ret, mfe, mae, event_ts)
VALUES ((SELECT id FROM decision WHERE data_snapshot_id = 'snap-outcome-unique' AND action = 'enter'),
        INTERVAL '1 day', 0.01, 0.004, 0.02, -0.01, '2026-01-06T21:00:00Z');

SELECT pg_temp.assert_true(
  (SELECT count(*) = 2 FROM outcome_window
    WHERE decision_id = (SELECT id FROM decision WHERE data_snapshot_id = 'snap-outcome-unique' AND action = 'enter')),
  'outcome_window must accept a second horizon for the same decision');

-- 4. A different decision with the same horizon is accepted.
INSERT INTO outcome_window (decision_id, horizon, fwd_ret, bench_ret, mfe, mae, event_ts)
VALUES ((SELECT id FROM decision WHERE data_snapshot_id = 'snap-outcome-unique' AND action = 'exit'),
        INTERVAL '5 days', 0.01, 0.004, 0.02, -0.01, '2026-01-12T21:00:00Z');

-- 5. INTERVAL '5 days' and make_interval(days => 5) are the same key: the second is refused.
SELECT pg_temp.assert_raises($q$
  INSERT INTO outcome_window (decision_id, horizon, fwd_ret, bench_ret, mfe, mae, event_ts)
  VALUES ((SELECT id FROM decision WHERE data_snapshot_id = 'snap-outcome-unique' AND action = 'enter'),
          make_interval(days => 5), 0.01, 0.004, 0.02, -0.01, '2026-01-12T21:00:00Z')
$q$, '23505');

-- 6. The unique index exists under its name, and the old non-unique index is gone.
SELECT pg_temp.assert_true(
  (SELECT count(*) = 1 FROM pg_indexes
    WHERE schemaname = 'public' AND tablename = 'outcome_window'
      AND indexname = 'outcome_window_decision_horizon_key'
      AND indexdef LIKE 'CREATE UNIQUE INDEX %'),
  'outcome_window_decision_horizon_key must be a unique index on outcome_window');

SELECT pg_temp.assert_true(
  NOT EXISTS (SELECT 1 FROM pg_indexes
               WHERE schemaname = 'public'
                 AND indexname = 'outcome_window_decision_id_horizon_idx'),
  'the old outcome_window_decision_id_horizon_idx index must be dropped');
