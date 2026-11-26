-- Schema checks for decision, market_snapshot and risk_check. Runs inside the runner's transaction.

-- Parent row for the decision foreign key (same valid insert as 0002).
INSERT INTO strategy_version (git_sha, config_hash, params, event_ts)
VALUES (repeat('a', 40), decode(repeat('ab', 32), 'hex'), '{}', '2026-01-02T00:00:00Z');

-- 1. One valid decision per action. The first one is the enter row, so min(id) is it.
INSERT INTO decision (strategy_version_id, symbol, action, intended_qty, expected_edge_bps,
                      expected_vol, features, feature_hash, data_snapshot_id, rng_seed,
                      reason_code, event_ts)
VALUES ((SELECT id FROM strategy_version WHERE git_sha = repeat('a', 40)),
        'SPY', 'enter', 1.5, 12.5, 0.15, '{"mom_12m": 0.1}', decode(repeat('ab', 32), 'hex'),
        'snap-1', 42, 'trend_up', '2026-01-05T21:00:00Z');

INSERT INTO decision (strategy_version_id, symbol, action, intended_qty, expected_edge_bps,
                      expected_vol, features, feature_hash, data_snapshot_id, rng_seed,
                      reason_code, event_ts)
VALUES ((SELECT id FROM strategy_version WHERE git_sha = repeat('a', 40)),
        'SPY', 'exit', 1.5, NULL, NULL, '{"mom_12m": 0.1}', decode(repeat('ab', 32), 'hex'),
        'snap-1', 42, 'trend_up', '2026-01-05T21:00:00Z'),
       ((SELECT id FROM strategy_version WHERE git_sha = repeat('a', 40)),
        'SPY', 'hold', 0, NULL, NULL, '{"mom_12m": 0.1}', decode(repeat('ab', 32), 'hex'),
        'snap-1', 42, 'trend_up', '2026-01-05T21:00:00Z'),
       ((SELECT id FROM strategy_version WHERE git_sha = repeat('a', 40)),
        'SPY', 'skip', 0, 3.0, 0.2, '{"mom_12m": 0.1}', decode(repeat('ab', 32), 'hex'),
        'snap-1', 42, 'trend_up', '2026-01-05T21:00:00Z');

SELECT pg_temp.assert_true(
  (SELECT count(*) = 4 FROM decision WHERE symbol = 'SPY' AND data_snapshot_id = 'snap-1'),
  'decision must accept all four actions');

SELECT pg_temp.assert_true(
  (SELECT ingest_ts IS NOT NULL FROM decision WHERE action = 'enter'),
  'decision.ingest_ts must default to clock_timestamp()');

-- 2. intended_qty is NUMERIC, so a fractional quantity reads back exactly.
INSERT INTO decision (strategy_version_id, symbol, action, intended_qty, features, feature_hash,
                      data_snapshot_id, rng_seed, reason_code, event_ts)
VALUES ((SELECT id FROM strategy_version WHERE git_sha = repeat('a', 40)),
        'QQQ', 'enter', 0.123456789, '{}', decode(repeat('ab', 32), 'hex'),
        'snap-2', 7, 'trend_up', '2026-01-05T21:00:00Z');

SELECT pg_temp.assert_true(
  (SELECT intended_qty = 0.123456789::numeric FROM decision WHERE symbol = 'QQQ'),
  'decision.intended_qty must store fractional values exactly');

-- 3. decision rejections.
SELECT pg_temp.assert_raises($q$
  INSERT INTO decision (strategy_version_id, symbol, action, intended_qty, features, feature_hash,
                        data_snapshot_id, rng_seed, reason_code, event_ts)
  VALUES ((SELECT id FROM strategy_version WHERE git_sha = repeat('a', 40)),
          'SPY', 'buy', 1, '{}', decode(repeat('ab', 32), 'hex'),
          'snap-1', 42, 'trend_up', '2026-01-05T21:00:00Z')
$q$, '23514');

SELECT pg_temp.assert_raises($q$
  INSERT INTO decision (strategy_version_id, symbol, action, intended_qty, features, feature_hash,
                        data_snapshot_id, rng_seed, reason_code, event_ts)
  VALUES ((SELECT id FROM strategy_version WHERE git_sha = repeat('a', 40)),
          '', 'enter', 1, '{}', decode(repeat('ab', 32), 'hex'),
          'snap-1', 42, 'trend_up', '2026-01-05T21:00:00Z')
$q$, '23514');

SELECT pg_temp.assert_raises($q$
  INSERT INTO decision (strategy_version_id, symbol, action, intended_qty, features, feature_hash,
                        data_snapshot_id, rng_seed, reason_code, event_ts)
  VALUES ((SELECT id FROM strategy_version WHERE git_sha = repeat('a', 40)),
          'SPY', 'enter', 1, '{}', decode(repeat('ab', 32), 'hex'),
          'snap-1', 42, '', '2026-01-05T21:00:00Z')
$q$, '23514');

SELECT pg_temp.assert_raises($q$
  INSERT INTO decision (strategy_version_id, symbol, action, intended_qty, features, feature_hash,
                        data_snapshot_id, rng_seed, reason_code, event_ts)
  VALUES ((SELECT id FROM strategy_version WHERE git_sha = repeat('a', 40)),
          'SPY', 'enter', 1, '{}', decode(repeat('ab', 31), 'hex'),
          'snap-1', 42, 'trend_up', '2026-01-05T21:00:00Z')
$q$, '23514');

SELECT pg_temp.assert_raises($q$
  INSERT INTO decision (strategy_version_id, symbol, action, intended_qty, features, feature_hash,
                        data_snapshot_id, rng_seed, reason_code, event_ts)
  VALUES (999999,
          'SPY', 'enter', 1, '{}', decode(repeat('ab', 32), 'hex'),
          'snap-1', 42, 'trend_up', '2026-01-05T21:00:00Z')
$q$, '23503');

SELECT pg_temp.assert_raises($q$
  INSERT INTO decision (strategy_version_id, symbol, action, intended_qty, features, feature_hash,
                        data_snapshot_id, rng_seed, reason_code, event_ts)
  VALUES ((SELECT id FROM strategy_version WHERE git_sha = repeat('a', 40)),
          'SPY', 'enter', 1, NULL, decode(repeat('ab', 32), 'hex'),
          'snap-1', 42, 'trend_up', '2026-01-05T21:00:00Z')
$q$, '23502');

SELECT pg_temp.assert_raises($q$
  INSERT INTO decision (strategy_version_id, symbol, action, intended_qty, features, feature_hash,
                        data_snapshot_id, rng_seed, reason_code, event_ts)
  VALUES ((SELECT id FROM strategy_version WHERE git_sha = repeat('a', 40)),
          'SPY', 'enter', 1, '{}', decode(repeat('ab', 32), 'hex'),
          'snap-1', NULL, 'trend_up', '2026-01-05T21:00:00Z')
$q$, '23502');

SELECT pg_temp.assert_raises($q$
  INSERT INTO decision (strategy_version_id, symbol, action, intended_qty, features, feature_hash,
                        data_snapshot_id, rng_seed, reason_code, event_ts)
  VALUES ((SELECT id FROM strategy_version WHERE git_sha = repeat('a', 40)),
          'SPY', 'enter', 1, '{}', decode(repeat('ab', 32), 'hex'),
          'snap-1', 42, 'trend_up', NULL)
$q$, '23502');

-- id is GENERATED ALWAYS, so an explicit id is rejected.
SELECT pg_temp.assert_raises($q$
  INSERT INTO decision (id, strategy_version_id, symbol, action, intended_qty, features,
                        feature_hash, data_snapshot_id, rng_seed, reason_code, event_ts)
  VALUES (424242, (SELECT id FROM strategy_version WHERE git_sha = repeat('a', 40)),
          'SPY', 'enter', 1, '{}', decode(repeat('ab', 32), 'hex'),
          'snap-1', 42, 'trend_up', '2026-01-05T21:00:00Z')
$q$, '428C9');

-- 4. market_snapshot: valid rows.
INSERT INTO market_snapshot (symbol, bid, ask, mid, spread_bps, adv, vol, regime_tag, event_ts)
VALUES ('SPY', 500.10, 500.12, 500.11, 0.4, 1000000, 0.15, 'calm', '2026-01-05T21:00:00Z');

INSERT INTO market_snapshot (symbol, event_ts)
VALUES ('SPY', '2026-01-05T21:00:00Z');

INSERT INTO market_snapshot (symbol, bid, ask, event_ts)
VALUES ('SPY', NULL, 500.12, '2026-01-05T21:00:00Z');

INSERT INTO market_snapshot (symbol, bid, ask, event_ts)
VALUES ('SPY', 500.12, 500.12, '2026-01-05T21:00:00Z');

SELECT pg_temp.assert_true(
  (SELECT count(*) = 4 FROM market_snapshot WHERE symbol = 'SPY'),
  'market_snapshot must accept full, sparse, one-sided and locked quotes');

-- 5. market_snapshot rejections.
SELECT pg_temp.assert_raises($q$
  INSERT INTO market_snapshot (symbol, bid, ask, event_ts)
  VALUES ('SPY', 500.13, 500.12, '2026-01-05T21:00:00Z')
$q$, '23514');

SELECT pg_temp.assert_raises($q$
  INSERT INTO market_snapshot (symbol, event_ts)
  VALUES ('', '2026-01-05T21:00:00Z')
$q$, '23514');

SELECT pg_temp.assert_raises($q$
  INSERT INTO market_snapshot (symbol, event_ts)
  VALUES (NULL, '2026-01-05T21:00:00Z')
$q$, '23502');

SELECT pg_temp.assert_raises($q$
  INSERT INTO market_snapshot (symbol, event_ts)
  VALUES ('SPY', NULL)
$q$, '23502');

-- 6. risk_check: one passing and one failing rule for the first decision (the enter row).
INSERT INTO risk_check (decision_id, rule, limit_value, actual_value, passed, event_ts)
VALUES
  ((SELECT min(id) FROM decision), 'max_position_pct', 0.25, 0.10, true, '2026-01-05T21:00:00Z'),
  ((SELECT min(id) FROM decision), 'per_trade_risk_pct', 0.005, 0.02, false, '2026-01-05T21:00:00Z');

SELECT pg_temp.assert_true(
  (SELECT count(*) = 1 FROM risk_check
    WHERE decision_id = (SELECT min(id) FROM decision) AND passed = false),
  'exactly one risk_check row for the decision must have passed = false');

-- 7. risk_check rejections.
SELECT pg_temp.assert_raises($q$
  INSERT INTO risk_check (decision_id, rule, limit_value, actual_value, passed, event_ts)
  VALUES ((SELECT min(id) FROM decision), '', 0.25, 0.10, true, '2026-01-05T21:00:00Z')
$q$, '23514');

SELECT pg_temp.assert_raises($q$
  INSERT INTO risk_check (decision_id, rule, limit_value, actual_value, passed, event_ts)
  VALUES (999999, 'max_position_pct', 0.25, 0.10, true, '2026-01-05T21:00:00Z')
$q$, '23503');

SELECT pg_temp.assert_raises($q$
  INSERT INTO risk_check (decision_id, rule, limit_value, actual_value, passed, event_ts)
  VALUES ((SELECT min(id) FROM decision), 'max_position_pct', 0.25, 0.10, NULL, '2026-01-05T21:00:00Z')
$q$, '23502');

SELECT pg_temp.assert_raises($q$
  INSERT INTO risk_check (decision_id, rule, limit_value, actual_value, passed, event_ts)
  VALUES ((SELECT min(id) FROM decision), 'max_position_pct', NULL, 0.10, true, '2026-01-05T21:00:00Z')
$q$, '23502');

SELECT pg_temp.assert_raises($q$
  INSERT INTO risk_check (decision_id, rule, limit_value, actual_value, passed, event_ts)
  VALUES ((SELECT min(id) FROM decision), 'max_position_pct', 0.25, NULL, true, '2026-01-05T21:00:00Z')
$q$, '23502');

-- 8. All three tables are append-only (checked after rows exist).
SELECT pg_temp.assert_append_only('decision');
SELECT pg_temp.assert_append_only('market_snapshot');
SELECT pg_temp.assert_append_only('risk_check');
