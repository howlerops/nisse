-- Schema checks for account_equity (0015_account_equity.sql). Runs inside the runner's
-- transaction, which is rolled back.

INSERT INTO strategy_version (git_sha, config_hash, params, event_ts)
VALUES (repeat('a', 40), decode(repeat('ab', 32), 'hex'), '{}', '2026-01-05T00:00:00Z');

-- 1. A valid insert for each environment. ingest_ts is filled in by default.
INSERT INTO account_equity (strategy_version_id, environment, session_date, equity, event_ts)
SELECT id, 'paper', DATE '2026-01-05', 100000.25, TIMESTAMPTZ '2026-01-05T21:10:00Z' FROM strategy_version
UNION ALL
SELECT id, 'live', DATE '2026-01-05', 0, TIMESTAMPTZ '2026-01-05T21:10:00Z' FROM strategy_version;

SELECT pg_temp.assert_true(
  (SELECT count(*) = 2 AND bool_and(ingest_ts >= '2020-01-01Z') FROM account_equity),
  'account_equity must accept paper and live rows and default ingest_ts');

-- 2. A correction is a second row for the same session; the newest id is the value.
INSERT INTO account_equity (strategy_version_id, environment, session_date, equity, event_ts)
SELECT id, 'paper', DATE '2026-01-05', 100001, TIMESTAMPTZ '2026-01-05T21:40:00Z' FROM strategy_version;

SELECT pg_temp.assert_true(
  (SELECT equity FROM account_equity
    WHERE environment = 'paper' AND session_date = '2026-01-05'
    ORDER BY id DESC LIMIT 1) = 100001,
  'the newest account_equity row for a session must hold its equity');

SELECT pg_temp.assert_true(
  EXISTS (
    SELECT 1 FROM pg_indexes
    WHERE schemaname = 'public'
      AND indexname = 'account_equity_version_session_idx'
      AND indexdef LIKE '%(strategy_version_id, session_date, id DESC)%'),
  'account_equity_version_session_idx must exist');

-- 3. Constraints: unknown environment, negative equity, unknown strategy version, nulls.
SELECT pg_temp.assert_raises($q$
  INSERT INTO account_equity (strategy_version_id, environment, session_date, equity, event_ts)
  SELECT id, 'shadow', DATE '2026-01-06', 1, TIMESTAMPTZ '2026-01-06T21:10:00Z' FROM strategy_version
$q$, '23514');

SELECT pg_temp.assert_raises($q$
  INSERT INTO account_equity (strategy_version_id, environment, session_date, equity, event_ts)
  SELECT id, 'paper', DATE '2026-01-06', -0.01, TIMESTAMPTZ '2026-01-06T21:10:00Z' FROM strategy_version
$q$, '23514');

SELECT pg_temp.assert_raises($q$
  INSERT INTO account_equity (strategy_version_id, environment, session_date, equity, event_ts)
  VALUES (-1, 'paper', DATE '2026-01-06', 1, '2026-01-06T21:10:00Z')
$q$, '23503');

SELECT pg_temp.assert_raises($q$
  INSERT INTO account_equity (strategy_version_id, environment, session_date, equity, event_ts)
  SELECT id, 'paper', NULL::date, 1, TIMESTAMPTZ '2026-01-06T21:10:00Z' FROM strategy_version
$q$, '23502');

-- 4. Append-only: no UPDATE, no DELETE.
SELECT pg_temp.assert_raises($q$ UPDATE account_equity SET equity = 1 $q$, '23001');
SELECT pg_temp.assert_raises($q$ DELETE FROM account_equity $q$, '23001');
