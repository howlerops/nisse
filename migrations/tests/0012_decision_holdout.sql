-- Schema, view and role checks for decision_holdout, agent_decision and nisse_agent
-- (0011_decision_holdout.sql). Runs inside the runner's transaction, which is rolled back.
-- While a restricted role is active, "must fail" checks are DO blocks that run each statement and
-- expect insufficient_privilege (42501). Assertions that call pg_temp helpers run after RESET ROLE.

-- Parent rows, as owner: a strategy_version and three decisions for one snapshot.
--   enter (decision 1) gets a hidden holdout row, exit (decision 2) a visible one, and
--   hold (decision 3) has no holdout row yet, so it is unassigned.
INSERT INTO strategy_version (git_sha, config_hash, params, event_ts)
VALUES (repeat('a', 40), decode(repeat('ab', 32), 'hex'), '{}', '2026-01-02T00:00:00Z');

INSERT INTO decision (strategy_version_id, symbol, action, intended_qty, expected_edge_bps,
                      expected_vol, features, feature_hash, data_snapshot_id, rng_seed,
                      reason_code, event_ts)
VALUES ((SELECT id FROM strategy_version WHERE git_sha = repeat('a', 40)),
        'SPY', 'enter', 1.5, 12.5, 0.15, '{"mom_12m": 0.1}', decode(repeat('ab', 32), 'hex'),
        'snap-holdout', 42, 'trend_up', '2026-01-05T21:00:00Z'),
       ((SELECT id FROM strategy_version WHERE git_sha = repeat('a', 40)),
        'SPY', 'exit', 1.5, NULL, NULL, '{"mom_12m": 0.1}', decode(repeat('ab', 32), 'hex'),
        'snap-holdout', 42, 'trend_up', '2026-01-05T21:00:00Z'),
       ((SELECT id FROM strategy_version WHERE git_sha = repeat('a', 40)),
        'SPY', 'hold', 0, NULL, NULL, '{"mom_12m": 0.1}', decode(repeat('ab', 32), 'hex'),
        'snap-holdout', 42, 'trend_up', '2026-01-05T21:00:00Z');

-- 1. Valid holdout rows: decision 1 hidden, decision 2 visible. ingest_ts is filled in by default.
INSERT INTO decision_holdout (decision_id, hidden, event_ts)
VALUES ((SELECT id FROM decision WHERE data_snapshot_id = 'snap-holdout' AND action = 'enter'),
        true, '2026-01-05T21:00:00Z'),
       ((SELECT id FROM decision WHERE data_snapshot_id = 'snap-holdout' AND action = 'exit'),
        false, '2026-01-05T21:00:00Z');

SELECT pg_temp.assert_true(
  (SELECT count(*) = 2 AND bool_and(ingest_ts >= '2020-01-01Z') FROM decision_holdout),
  'decision_holdout must accept two valid rows and default ingest_ts');

-- 2. Constraints. A second row for a decision is refused with 23505, even with the same value.
SELECT pg_temp.assert_raises($q$
  INSERT INTO decision_holdout (decision_id, hidden, event_ts)
  VALUES ((SELECT id FROM decision WHERE data_snapshot_id = 'snap-holdout' AND action = 'enter'),
          true, '2026-01-05T21:00:00Z')
$q$, '23505');

SELECT pg_temp.assert_raises($q$
  INSERT INTO decision_holdout (decision_id, hidden, event_ts)
  VALUES ((SELECT id FROM decision WHERE data_snapshot_id = 'snap-holdout' AND action = 'enter'),
          false, '2026-01-05T21:00:00Z')
$q$, '23505');

SELECT pg_temp.assert_raises($q$
  INSERT INTO decision_holdout (decision_id, hidden, event_ts)
  VALUES (424242, true, '2026-01-05T21:00:00Z')
$q$, '23503');

SELECT pg_temp.assert_raises($q$
  INSERT INTO decision_holdout (decision_id, hidden, event_ts)
  VALUES (NULL, true, '2026-01-05T21:00:00Z')
$q$, '23502');

SELECT pg_temp.assert_raises($q$
  INSERT INTO decision_holdout (decision_id, hidden, event_ts)
  VALUES ((SELECT id FROM decision WHERE data_snapshot_id = 'snap-holdout' AND action = 'hold'),
          NULL, '2026-01-05T21:00:00Z')
$q$, '23502');

SELECT pg_temp.assert_raises($q$
  INSERT INTO decision_holdout (decision_id, hidden, event_ts)
  VALUES ((SELECT id FROM decision WHERE data_snapshot_id = 'snap-holdout' AND action = 'hold'),
          true, NULL)
$q$, '23502');

-- id is GENERATED ALWAYS, so an explicit id is rejected.
SELECT pg_temp.assert_raises($q$
  INSERT INTO decision_holdout (id, decision_id, hidden, event_ts)
  VALUES (424242, (SELECT id FROM decision WHERE data_snapshot_id = 'snap-holdout' AND action = 'hold'),
          true, '2026-01-05T21:00:00Z')
$q$, '428C9');

-- 3. Append-only (checked after rows exist).
SELECT pg_temp.assert_append_only('decision_holdout');

-- 4. The view, as owner: exactly decision 2 (exit) is visible. Decisions 1 and 3 are absent.
SELECT pg_temp.assert_true(
  (SELECT array_agg(id ORDER BY id) FROM agent_decision WHERE data_snapshot_id = 'snap-holdout')
  = ARRAY[(SELECT id FROM decision WHERE data_snapshot_id = 'snap-holdout' AND action = 'exit')],
  'agent_decision must return exactly the visible decision (the exit row)');

SELECT pg_temp.assert_true(
  (SELECT count(*) = 0 FROM agent_decision
    WHERE id IN (SELECT id FROM decision
                  WHERE data_snapshot_id = 'snap-holdout' AND action IN ('enter', 'hold'))),
  'agent_decision must not return a hidden decision or one with no holdout row');

-- The view exposes exactly the decision columns, without hidden or ingest_ts.
SELECT pg_temp.assert_true(
  (SELECT array_agg(column_name::text ORDER BY ordinal_position)
     FROM information_schema.columns
    WHERE table_schema = 'public' AND table_name = 'agent_decision')
  = ARRAY['id', 'strategy_version_id', 'symbol', 'action', 'intended_qty', 'expected_edge_bps',
          'expected_vol', 'features', 'feature_hash', 'data_snapshot_id', 'rng_seed',
          'reason_code', 'event_ts'],
  'agent_decision must expose the decision columns and no ingest_ts or hidden');

-- 5. Roles, as owner: nisse_agent exists, cannot log in, and is in neither nisse_reader nor
--    nisse_writer. Its only grants are checked below.
SELECT pg_temp.assert_true(
  (SELECT count(*) = 1 AND bool_and(NOT rolcanlogin) FROM pg_roles WHERE rolname = 'nisse_agent'),
  'nisse_agent must exist and be NOLOGIN');

SELECT pg_temp.assert_true(
  NOT pg_has_role('nisse_agent', 'nisse_reader', 'MEMBER')
  AND NOT pg_has_role('nisse_agent', 'nisse_writer', 'MEMBER'),
  'nisse_agent must not be a member of nisse_reader or nisse_writer');

-- 6. The agent's privileges, from the catalog (as owner).
SELECT pg_temp.assert_true(
  has_table_privilege('nisse_agent', 'agent_decision', 'SELECT'),
  'nisse_agent must SELECT agent_decision');

SELECT pg_temp.assert_true(
  NOT has_table_privilege('nisse_agent', 'agent_decision', 'INSERT')
  AND NOT has_table_privilege('nisse_agent', 'agent_decision', 'UPDATE')
  AND NOT has_table_privilege('nisse_agent', 'agent_decision', 'DELETE'),
  'nisse_agent must not INSERT, UPDATE or DELETE agent_decision');

SELECT pg_temp.assert_true(
  NOT has_table_privilege('nisse_agent', 'decision', 'SELECT')
  AND NOT has_table_privilege('nisse_agent', 'decision_holdout', 'SELECT')
  AND NOT has_table_privilege('nisse_agent', 'fill', 'SELECT')
  AND NOT has_table_privilege('nisse_agent', 'trial_registry', 'SELECT'),
  'nisse_agent must not SELECT any base table');

-- 7. As the agent: the view is readable and returns one row; every base table and every write is
--    refused with 42501. Each refused statement is run, and must raise insufficient_privilege.
SET LOCAL ROLE nisse_agent;

DO $$
DECLARE
  n bigint;
BEGIN
  SELECT count(*) INTO STRICT n FROM agent_decision WHERE data_snapshot_id = 'snap-holdout';
  IF n <> 1 THEN
    RAISE EXCEPTION 'assertion failed: nisse_agent must read exactly 1 visible decision, got %', n;
  END IF;
END $$;

DO $$
DECLARE
  stmt text;
BEGIN
  FOREACH stmt IN ARRAY ARRAY[
    'SELECT count(*) FROM decision',
    'SELECT count(*) FROM decision_holdout',
    'SELECT count(*) FROM fill',
    'SELECT count(*) FROM trial_registry',
    'INSERT INTO decision_holdout (decision_id, hidden, event_ts) VALUES (1, true, now())',
    'UPDATE decision_holdout SET hidden = NOT hidden',
    'CREATE TABLE sneaky_agent (x int)'
  ] LOOP
    BEGIN
      EXECUTE stmt;
    EXCEPTION WHEN insufficient_privilege THEN
      CONTINUE;
    END;
    RAISE EXCEPTION 'expected insufficient_privilege (42501): nisse_agent ran %', stmt;
  END LOOP;
END $$;

-- Writes to the view itself. agent_decision is a join, so Postgres does not treat it as
-- automatically updatable: the rewriter refuses INSERT, UPDATE and DELETE with 55000 ("cannot
-- insert into view", observed on PostgreSQL) before the privilege check runs. The expected code is
-- the one observed. The grant itself is pinned by has_table_privilege (section 6): no INSERT,
-- UPDATE or DELETE on agent_decision.
DO $$
DECLARE
  r record;
BEGIN
  FOR r IN
    SELECT * FROM (VALUES
      ('INSERT INTO agent_decision (id) VALUES (1)', '55000'),
      ('UPDATE agent_decision SET symbol = symbol', '55000'),
      ('DELETE FROM agent_decision', '55000')
    ) AS t(stmt, want)
  LOOP
    BEGIN
      EXECUTE r.stmt;
    EXCEPTION WHEN OTHERS THEN
      IF SQLSTATE = r.want THEN
        CONTINUE;
      END IF;
      RAISE EXCEPTION 'expected SQLSTATE % but got % (%) from: %', r.want, SQLSTATE, SQLERRM, r.stmt;
    END;
    RAISE EXCEPTION 'nisse_agent write to the view succeeded: %', r.stmt;
  END LOOP;
END $$;

RESET ROLE;

-- 8. As the writer: the granted columns insert a row for the unassigned decision (hold). Supplying
--    ingest_ts is refused (42501), checked first, before the valid row exists.
SET LOCAL ROLE nisse_writer;

DO $$
DECLARE
  stmt text;
BEGIN
  FOREACH stmt IN ARRAY ARRAY[
    'INSERT INTO decision_holdout (decision_id, hidden, event_ts, ingest_ts)
     VALUES ((SELECT id FROM decision WHERE data_snapshot_id = ''snap-holdout'' AND action = ''hold''),
             true, ''2026-01-05T21:00:00Z'', ''1970-01-01'')'
  ] LOOP
    BEGIN
      EXECUTE stmt;
    EXCEPTION WHEN insufficient_privilege THEN
      CONTINUE;
    END;
    RAISE EXCEPTION 'expected insufficient_privilege (42501): writer ran %', stmt;
  END LOOP;
END $$;

INSERT INTO decision_holdout (decision_id, hidden, event_ts)
VALUES ((SELECT id FROM decision WHERE data_snapshot_id = 'snap-holdout' AND action = 'hold'),
        true, '2026-01-05T21:00:00Z');

RESET ROLE;

SELECT pg_temp.assert_true(
  (SELECT count(*) = 3 FROM decision_holdout
    WHERE decision_id IN (SELECT id FROM decision WHERE data_snapshot_id = 'snap-holdout')),
  'decision_holdout must hold the three rows of this test, including the writer''s');

SELECT pg_temp.assert_true(
  (SELECT count(*) = 1 FROM agent_decision WHERE data_snapshot_id = 'snap-holdout'),
  'the writer''s hidden row must keep the view at exactly one visible decision');

-- 9. As the reader: SELECT works, INSERT is refused (42501).
SET LOCAL ROLE nisse_reader;

DO $$ BEGIN
  PERFORM count(*) FROM decision_holdout;
END $$;

DO $$
DECLARE
  stmt text;
BEGIN
  FOREACH stmt IN ARRAY ARRAY[
    'INSERT INTO decision_holdout (decision_id, hidden, event_ts) VALUES (1, true, now())'
  ] LOOP
    BEGIN
      EXECUTE stmt;
    EXCEPTION WHEN insufficient_privilege THEN
      CONTINUE;
    END;
    RAISE EXCEPTION 'expected insufficient_privilege (42501): reader ran %', stmt;
  END LOOP;
END $$;

RESET ROLE;
