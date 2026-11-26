-- Schema checks for strategy_version and trial_registry. Runs inside the runner's transaction.

-- 1. Valid strategy_version row; ingest_ts is filled in by default.
INSERT INTO strategy_version (git_sha, config_hash, params, event_ts)
VALUES (repeat('a', 40), decode(repeat('ab', 32), 'hex'), '{}', '2026-01-02T00:00:00Z');

SELECT pg_temp.assert_true(
  (SELECT ingest_ts IS NOT NULL FROM strategy_version WHERE git_sha = repeat('a', 40)),
  'strategy_version.ingest_ts must default to clock_timestamp()');

-- 2. Child version pointing at the first row; a missing parent is a foreign-key violation.
INSERT INTO strategy_version (git_sha, config_hash, params, parent_id, event_ts)
VALUES (repeat('b', 40), decode(repeat('cd', 32), 'hex'), '{}',
        (SELECT id FROM strategy_version WHERE git_sha = repeat('a', 40)),
        '2026-01-02T00:00:00Z');

SELECT pg_temp.assert_raises($q$
  INSERT INTO strategy_version (git_sha, config_hash, params, parent_id, event_ts)
  VALUES (repeat('e', 40), decode(repeat('ef', 32), 'hex'), '{}', 999999, '2026-01-02T00:00:00Z')
$q$, '23503');

-- 3. CHECK violations: git_sha shape and config_hash length.
SELECT pg_temp.assert_raises($q$
  INSERT INTO strategy_version (git_sha, config_hash, params, event_ts)
  VALUES (repeat('a', 39), decode(repeat('ab', 32), 'hex'), '{}', '2026-01-02T00:00:00Z')
$q$, '23514');

SELECT pg_temp.assert_raises($q$
  INSERT INTO strategy_version (git_sha, config_hash, params, event_ts)
  VALUES (repeat('A', 40), decode(repeat('ab', 32), 'hex'), '{}', '2026-01-02T00:00:00Z')
$q$, '23514');

SELECT pg_temp.assert_raises($q$
  INSERT INTO strategy_version (git_sha, config_hash, params, event_ts)
  VALUES (repeat('c', 40), decode(repeat('ab', 31), 'hex'), '{}', '2026-01-02T00:00:00Z')
$q$, '23514');

-- 4. UNIQUE (git_sha, config_hash): same pair as the first row.
SELECT pg_temp.assert_raises($q$
  INSERT INTO strategy_version (git_sha, config_hash, params, event_ts)
  VALUES (repeat('a', 40), decode(repeat('ab', 32), 'hex'), '{}', '2026-01-02T00:00:00Z')
$q$, '23505');

-- 5. NOT NULL on params and event_ts.
SELECT pg_temp.assert_raises($q$
  INSERT INTO strategy_version (git_sha, config_hash, params, event_ts)
  VALUES (repeat('c', 40), decode(repeat('ab', 32), 'hex'), NULL, '2026-01-02T00:00:00Z')
$q$, '23502');

SELECT pg_temp.assert_raises($q$
  INSERT INTO strategy_version (git_sha, config_hash, params, event_ts)
  VALUES (repeat('c', 40), decode(repeat('ab', 32), 'hex'), '{}', NULL)
$q$, '23502');

-- 6. id is GENERATED ALWAYS, so an explicit id is rejected.
SELECT pg_temp.assert_raises($q$
  INSERT INTO strategy_version (id, git_sha, config_hash, params, event_ts)
  VALUES (424242, repeat('c', 40), decode(repeat('ab', 32), 'hex'), '{}', '2026-01-02T00:00:00Z')
$q$, '428C9');

-- 7. One valid trial per kind: one with NULL result, two with a result.
INSERT INTO trial_registry (strategy_version_id, kind, data_range, result, event_ts)
VALUES
  ((SELECT id FROM strategy_version WHERE git_sha = repeat('a', 40)),
   'backtest', tstzrange('2020-01-01Z', '2025-01-01Z'), NULL, '2026-01-03T00:00:00Z'),
  ((SELECT id FROM strategy_version WHERE git_sha = repeat('a', 40)),
   'shadow', tstzrange('2020-01-01Z', '2025-01-01Z'), '{"sharpe": 0.4}', '2026-01-03T00:00:00Z'),
  ((SELECT id FROM strategy_version WHERE git_sha = repeat('a', 40)),
   'live', tstzrange('2020-01-01Z', '2025-01-01Z'), '{"sharpe": 0.1}', '2026-01-03T00:00:00Z');

-- 8. trial_registry rejections.
SELECT pg_temp.assert_raises($q$
  INSERT INTO trial_registry (strategy_version_id, kind, data_range, event_ts)
  VALUES ((SELECT id FROM strategy_version WHERE git_sha = repeat('a', 40)),
          'paper', tstzrange('2020-01-01Z', '2025-01-01Z'), '2026-01-03T00:00:00Z')
$q$, '23514');

SELECT pg_temp.assert_raises($q$
  INSERT INTO trial_registry (strategy_version_id, kind, data_range, event_ts)
  VALUES ((SELECT id FROM strategy_version WHERE git_sha = repeat('a', 40)),
          'backtest', 'empty'::tstzrange, '2026-01-03T00:00:00Z')
$q$, '23514');

SELECT pg_temp.assert_raises($q$
  INSERT INTO trial_registry (strategy_version_id, kind, data_range, event_ts)
  VALUES (999999, 'backtest', tstzrange('2020-01-01Z', '2025-01-01Z'), '2026-01-03T00:00:00Z')
$q$, '23503');

SELECT pg_temp.assert_raises($q$
  INSERT INTO trial_registry (strategy_version_id, kind, data_range, event_ts)
  VALUES ((SELECT id FROM strategy_version WHERE git_sha = repeat('a', 40)),
          'backtest', NULL, '2026-01-03T00:00:00Z')
$q$, '23502');

-- 9. The lifetime trial count is the number of valid rows inserted in step 7.
SELECT pg_temp.assert_true(
  (SELECT count(*) = 3 FROM trial_registry),
  'trial_registry must hold exactly the 3 valid trials inserted by this test');

-- 10. Both tables are append-only (checked after rows exist).
SELECT pg_temp.assert_append_only('strategy_version');
SELECT pg_temp.assert_append_only('trial_registry');
