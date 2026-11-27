-- Schema checks for engine_step (0013_engine_step.sql). Runs inside the runner's transaction,
-- which is rolled back.

-- 1. A valid insert for every allowed status. ingest_ts is filled in by default.
INSERT INTO engine_step (run_key, step, status, detail, event_ts)
VALUES
  ('session-close/2026-01-05', 'ingest',    'started', '{}', '2026-01-05T21:10:00Z'),
  ('session-close/2026-01-05', 'ingest',    'done',    '{"bars": 390}', '2026-01-05T21:11:00Z'),
  ('session-close/2026-01-05', 'rebalance', 'failed',  '{"error": "timeout"}', '2026-01-05T21:12:00Z');

SELECT pg_temp.assert_true(
  (SELECT count(DISTINCT status) = 3 AND count(*) = 3 FROM engine_step),
  'engine_step must accept each of the three statuses');

SELECT pg_temp.assert_true(
  (SELECT bool_and(ingest_ts >= '2020-01-01Z') FROM engine_step),
  'engine_step.ingest_ts must default to clock_timestamp()');

-- 2. The newest status of a (run_key, step) is the last row inserted for it.
INSERT INTO engine_step (run_key, step, status, detail, event_ts)
VALUES ('session-close/2026-01-06', 'rebalance', 'started', '{}', '2026-01-06T21:10:00Z'),
       ('session-close/2026-01-06', 'rebalance', 'failed',  '{}', '2026-01-06T21:11:00Z'),
       ('session-close/2026-01-06', 'rebalance', 'done',    '{}', '2026-01-06T21:12:00Z');

SELECT pg_temp.assert_true(
  (SELECT status FROM engine_step
    WHERE run_key = 'session-close/2026-01-06' AND step = 'rebalance'
    ORDER BY id DESC LIMIT 1) = 'done',
  'the newest engine_step row for a (run_key, step) must hold its status');

SELECT pg_temp.assert_true(
  EXISTS (
    SELECT 1 FROM pg_indexes
    WHERE schemaname = 'public'
      AND indexname = 'engine_step_run_key_step_idx'
      AND indexdef LIKE '%(run_key, step, id DESC)%'),
  'engine_step_run_key_step_idx on (run_key, step, id DESC) must exist');

-- 3. engine_step constraints.
SELECT pg_temp.assert_raises($q$
  INSERT INTO engine_step (run_key, step, status, detail, event_ts)
  VALUES ('', 'ingest', 'started', '{}', '2026-01-05T21:00:00Z')
$q$, '23514');

SELECT pg_temp.assert_raises($q$
  INSERT INTO engine_step (run_key, step, status, detail, event_ts)
  VALUES ('session-close/2026-01-05', '', 'started', '{}', '2026-01-05T21:00:00Z')
$q$, '23514');

SELECT pg_temp.assert_raises($q$
  INSERT INTO engine_step (run_key, step, status, detail, event_ts)
  VALUES ('session-close/2026-01-05', 'ingest', 'skipped', '{}', '2026-01-05T21:00:00Z')
$q$, '23514');

SELECT pg_temp.assert_raises($q$
  INSERT INTO engine_step (run_key, step, status, detail, event_ts)
  VALUES ('session-close/2026-01-05', 'ingest', 'Done', '{}', '2026-01-05T21:00:00Z')
$q$, '23514');

SELECT pg_temp.assert_raises($q$
  INSERT INTO engine_step (run_key, step, status, detail, event_ts)
  VALUES ('session-close/2026-01-05', 'ingest', 'started', '[]', '2026-01-05T21:00:00Z')
$q$, '23514');

SELECT pg_temp.assert_raises($q$
  INSERT INTO engine_step (run_key, step, status, detail, event_ts)
  VALUES ('session-close/2026-01-05', 'ingest', 'started', '"done"', '2026-01-05T21:00:00Z')
$q$, '23514');

SELECT pg_temp.assert_raises($q$
  INSERT INTO engine_step (run_key, step, status, detail, event_ts)
  VALUES ('session-close/2026-01-05', 'ingest', 'started', NULL, '2026-01-05T21:00:00Z')
$q$, '23502');

SELECT pg_temp.assert_raises($q$
  INSERT INTO engine_step (run_key, step, status, detail, event_ts)
  VALUES (NULL, 'ingest', 'started', '{}', '2026-01-05T21:00:00Z')
$q$, '23502');

SELECT pg_temp.assert_raises($q$
  INSERT INTO engine_step (run_key, step, status, detail, event_ts)
  VALUES ('session-close/2026-01-05', NULL, 'started', '{}', '2026-01-05T21:00:00Z')
$q$, '23502');

SELECT pg_temp.assert_raises($q$
  INSERT INTO engine_step (run_key, step, status, detail, event_ts)
  VALUES ('session-close/2026-01-05', 'ingest', NULL, '{}', '2026-01-05T21:00:00Z')
$q$, '23502');

SELECT pg_temp.assert_raises($q$
  INSERT INTO engine_step (run_key, step, status, detail, event_ts)
  VALUES ('session-close/2026-01-05', 'ingest', 'started', '{}', NULL)
$q$, '23502');

-- 4. id is GENERATED ALWAYS, so an explicit id is rejected.
SELECT pg_temp.assert_raises($q$
  INSERT INTO engine_step (id, run_key, step, status, detail, event_ts)
  VALUES (424242, 'session-close/2026-01-05', 'ingest', 'started', '{}', '2026-01-05T21:00:00Z')
$q$, '428C9');

-- 5. Append-only (checked after rows exist): UPDATE, DELETE and TRUNCATE are refused.
SELECT pg_temp.assert_append_only('engine_step');

-- 6. As the writer: an insert naming its granted columns succeeds. Supplying ingest_ts is
--    refused (42501), as are UPDATE and DELETE.
SET LOCAL ROLE nisse_writer;

INSERT INTO engine_step (run_key, step, status, detail, event_ts)
VALUES ('session-close/2026-01-07', 'ingest', 'started', '{}', '2026-01-07T21:10:00Z');

DO $$ BEGIN
  BEGIN
    INSERT INTO engine_step (run_key, step, status, detail, event_ts, ingest_ts)
    VALUES ('session-close/2026-01-07', 'ingest', 'done', '{}', '2026-01-07T21:11:00Z', '1970-01-01');
  EXCEPTION WHEN insufficient_privilege THEN RETURN;
  END;
  RAISE EXCEPTION 'expected insufficient_privilege (42501): writer INSERT supplying ingest_ts into engine_step';
END $$;

DO $$ BEGIN
  BEGIN
    UPDATE engine_step SET status = 'done';
  EXCEPTION WHEN insufficient_privilege THEN RETURN;
  END;
  RAISE EXCEPTION 'expected insufficient_privilege (42501): UPDATE engine_step';
END $$;

DO $$ BEGIN
  BEGIN
    DELETE FROM engine_step;
  EXCEPTION WHEN insufficient_privilege THEN RETURN;
  END;
  RAISE EXCEPTION 'expected insufficient_privilege (42501): DELETE FROM engine_step';
END $$;

RESET ROLE;

-- 7. As the reader: SELECT works; INSERT is refused (42501).
SET LOCAL ROLE nisse_reader;

DO $$ BEGIN
  PERFORM count(*) FROM engine_step;
END $$;

DO $$ BEGIN
  BEGIN
    INSERT INTO engine_step (run_key, step, status, detail, event_ts)
    VALUES ('session-close/2026-01-07', 'ingest', 'started', '{}', '2026-01-07T21:10:00Z');
  EXCEPTION WHEN insufficient_privilege THEN RETURN;
  END;
  RAISE EXCEPTION 'expected insufficient_privilege (42501): reader INSERT INTO engine_step';
END $$;

RESET ROLE;

-- 8. As owner: this test's own rows are all there (3 + 3 + 1 writer row).
SELECT pg_temp.assert_true(
  (SELECT count(*) = 7 FROM engine_step),
  'engine_step must hold exactly the 7 rows inserted by this test');
