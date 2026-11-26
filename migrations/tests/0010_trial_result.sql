-- Schema checks for trial_result. Runs inside the runner's transaction.

-- Two trials to attach results to. Registered first: a trial is registered before it runs.
INSERT INTO strategy_version (git_sha, config_hash, params, event_ts)
VALUES (repeat('a', 40), decode(repeat('ab', 32), 'hex'), '{}', '2026-01-02T00:00:00Z');

INSERT INTO trial_registry (strategy_version_id, kind, data_range, event_ts)
VALUES
  ((SELECT id FROM strategy_version WHERE git_sha = repeat('a', 40)),
   'backtest', tstzrange('2020-01-01Z', '2025-01-01Z'), '2026-01-03T00:00:00Z'),
  ((SELECT id FROM strategy_version WHERE git_sha = repeat('a', 40)),
   'shadow', tstzrange('2020-01-01Z', '2025-01-01Z'), '2026-01-03T00:00:00Z');

-- 1. A valid result; ingest_ts is filled in by default.
INSERT INTO trial_result (trial_id, result, event_ts)
VALUES ((SELECT id FROM trial_registry WHERE kind = 'backtest'),
        '{"sharpe": 0.4, "notes": null}', '2026-01-04T00:00:00Z');

SELECT pg_temp.assert_true(
  (SELECT ingest_ts IS NOT NULL FROM trial_result
    WHERE trial_id = (SELECT id FROM trial_registry WHERE kind = 'backtest')),
  'trial_result.ingest_ts must default to clock_timestamp()');

-- 2. At most one result per trial: a second result for the same trial is a unique violation.
SELECT pg_temp.assert_raises($q$
  INSERT INTO trial_result (trial_id, result, event_ts)
  VALUES ((SELECT id FROM trial_registry WHERE kind = 'backtest'),
          '{"sharpe": 0.1}', '2026-01-04T00:00:00Z')
$q$, '23505');

-- 3. A result must name a registered trial.
SELECT pg_temp.assert_raises($q$
  INSERT INTO trial_result (trial_id, result, event_ts)
  VALUES (999999, '{"sharpe": 0.1}', '2026-01-04T00:00:00Z')
$q$, '23503');

-- 4. NOT NULL on result and event_ts.
SELECT pg_temp.assert_raises($q$
  INSERT INTO trial_result (trial_id, result, event_ts)
  VALUES ((SELECT id FROM trial_registry WHERE kind = 'shadow'), NULL, '2026-01-04T00:00:00Z')
$q$, '23502');

SELECT pg_temp.assert_raises($q$
  INSERT INTO trial_result (trial_id, result, event_ts)
  VALUES ((SELECT id FROM trial_registry WHERE kind = 'shadow'), '{}', NULL)
$q$, '23502');

-- 5. id is GENERATED ALWAYS, so an explicit id is rejected.
SELECT pg_temp.assert_raises($q$
  INSERT INTO trial_result (id, trial_id, result, event_ts)
  VALUES (424242, (SELECT id FROM trial_registry WHERE kind = 'shadow'), '{}',
          '2026-01-04T00:00:00Z')
$q$, '428C9');

-- 6. Append-only (checked after a row exists).
SELECT pg_temp.assert_append_only('trial_result');

-- 7. As the writer: an insert naming its columns succeeds for a second trial; UPDATE fails
--    with insufficient_privilege (42501).
SET LOCAL ROLE nisse_writer;

INSERT INTO trial_result (trial_id, result, event_ts)
VALUES ((SELECT id FROM trial_registry WHERE kind = 'shadow'),
        '{"sharpe": 0.2}', '2026-01-04T00:00:00Z');

DO $$ BEGIN
  BEGIN
    UPDATE trial_result SET result = '{}';
  EXCEPTION WHEN insufficient_privilege THEN RETURN;
  END;
  RAISE EXCEPTION 'expected insufficient_privilege (42501): UPDATE trial_result';
END $$;

RESET ROLE;

-- 8. The trial count is the row count of trial_registry, untouched by the results.
SELECT pg_temp.assert_true(
  (SELECT count(*) = 2 FROM trial_registry),
  'trial_registry must still hold exactly the 2 trials registered by this test');

SELECT pg_temp.assert_true(
  (SELECT count(*) = 2 FROM trial_result),
  'trial_result must hold exactly the 2 results inserted by this test');
