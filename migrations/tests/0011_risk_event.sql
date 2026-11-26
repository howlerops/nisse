-- Schema checks for risk_event and heartbeat (0010_risk_event.sql). Runs inside the runner's
-- transaction, which is rolled back.

-- 1. A valid insert for every allowed kind, in fold order. ingest_ts is filled in by default.
INSERT INTO risk_event (kind, detail, event_ts)
VALUES
  ('equity_peak',   '{"peak": "100000"}', '2026-01-05T21:00:00Z'),
  ('drawdown_mode', '{"mode": "halved", "equity": "89000", "reset": false}', '2026-01-06T21:00:00Z'),
  ('kill_switch',   '{"engaged": true, "by": "test"}', '2026-01-07T21:00:00Z'),
  ('limits_loaded', '{"hash": "abcd", "limits": {}}', '2026-01-08T21:00:00Z');

SELECT pg_temp.assert_true(
  (SELECT count(DISTINCT kind) = 4 AND count(*) = 4 FROM risk_event),
  'risk_event must accept each of the four kinds');

SELECT pg_temp.assert_true(
  (SELECT array_agg(kind ORDER BY id)
     = ARRAY['equity_peak', 'drawdown_mode', 'kill_switch', 'limits_loaded']
   FROM risk_event),
  'risk_event rows must come back in id order');

SELECT pg_temp.assert_true(
  (SELECT bool_and(ingest_ts >= '2020-01-01Z') FROM risk_event),
  'risk_event.ingest_ts must default to clock_timestamp()');

-- 2. A valid heartbeat insert: two rows for one source, so "newest for a source" is testable.
INSERT INTO heartbeat (source, event_ts)
VALUES ('engine', '2026-01-05T21:00:00Z'),
       ('engine', '2026-01-05T21:05:00Z');

SELECT pg_temp.assert_true(
  (SELECT count(*) = 2 AND bool_and(ingest_ts >= '2020-01-01Z') FROM heartbeat),
  'heartbeat must accept a valid row and default ingest_ts');

SELECT pg_temp.assert_true(
  (SELECT event_ts FROM heartbeat WHERE source = 'engine' ORDER BY event_ts DESC LIMIT 1)
  = '2026-01-05T21:05:00Z',
  'the newest heartbeat for a source must be the one with the latest event_ts');

SELECT pg_temp.assert_true(
  EXISTS (
    SELECT 1 FROM pg_indexes
    WHERE schemaname = 'public'
      AND indexname = 'heartbeat_source_event_ts_idx'
      AND indexdef LIKE '%(source, event_ts DESC)%'),
  'heartbeat_source_event_ts_idx on (source, event_ts DESC) must exist');

-- 3. risk_event constraints.
SELECT pg_temp.assert_raises($q$
  INSERT INTO risk_event (kind, detail, event_ts)
  VALUES ('fill', '{}', '2026-01-05T21:00:00Z')
$q$, '23514');

SELECT pg_temp.assert_raises($q$
  INSERT INTO risk_event (kind, detail, event_ts)
  VALUES ('Kill_Switch', '{}', '2026-01-05T21:00:00Z')
$q$, '23514');

SELECT pg_temp.assert_raises($q$
  INSERT INTO risk_event (kind, detail, event_ts)
  VALUES ('kill_switch', '[]', '2026-01-05T21:00:00Z')
$q$, '23514');

SELECT pg_temp.assert_raises($q$
  INSERT INTO risk_event (kind, detail, event_ts)
  VALUES ('kill_switch', '"engaged"', '2026-01-05T21:00:00Z')
$q$, '23514');

SELECT pg_temp.assert_raises($q$
  INSERT INTO risk_event (kind, detail, event_ts)
  VALUES ('kill_switch', '3', '2026-01-05T21:00:00Z')
$q$, '23514');

SELECT pg_temp.assert_raises($q$
  INSERT INTO risk_event (kind, detail, event_ts)
  VALUES (NULL, '{}', '2026-01-05T21:00:00Z')
$q$, '23502');

SELECT pg_temp.assert_raises($q$
  INSERT INTO risk_event (kind, detail, event_ts)
  VALUES ('kill_switch', NULL, '2026-01-05T21:00:00Z')
$q$, '23502');

SELECT pg_temp.assert_raises($q$
  INSERT INTO risk_event (kind, detail, event_ts)
  VALUES ('kill_switch', '{}', NULL)
$q$, '23502');

-- 4. heartbeat constraints.
SELECT pg_temp.assert_raises($q$
  INSERT INTO heartbeat (source, event_ts)
  VALUES ('', '2026-01-05T21:00:00Z')
$q$, '23514');

SELECT pg_temp.assert_raises($q$
  INSERT INTO heartbeat (source, event_ts)
  VALUES (NULL, '2026-01-05T21:00:00Z')
$q$, '23502');

SELECT pg_temp.assert_raises($q$
  INSERT INTO heartbeat (source, event_ts)
  VALUES ('engine', NULL)
$q$, '23502');

-- 5. id is GENERATED ALWAYS, so an explicit id is rejected for both tables.
SELECT pg_temp.assert_raises($q$
  INSERT INTO risk_event (id, kind, detail, event_ts)
  VALUES (424242, 'kill_switch', '{}', '2026-01-05T21:00:00Z')
$q$, '428C9');

SELECT pg_temp.assert_raises($q$
  INSERT INTO heartbeat (id, source, event_ts)
  VALUES (424242, 'engine', '2026-01-05T21:00:00Z')
$q$, '428C9');

-- 6. Append-only (checked after rows exist).
SELECT pg_temp.assert_append_only('risk_event');
SELECT pg_temp.assert_append_only('heartbeat');

-- 7. As the writer: an insert naming its granted columns succeeds for both tables. Supplying
--    ingest_ts is refused (42501), as is UPDATE and DELETE.
SET LOCAL ROLE nisse_writer;

INSERT INTO risk_event (kind, detail, event_ts)
VALUES ('drawdown_mode', '{"mode": "flat", "equity": "70000", "reset": true}',
        '2026-01-09T21:00:00Z');

INSERT INTO heartbeat (source, event_ts)
VALUES ('nisse-engine', '2026-01-09T21:00:00Z');

DO $$ BEGIN
  BEGIN
    INSERT INTO risk_event (kind, detail, event_ts, ingest_ts)
    VALUES ('kill_switch', '{}', '2026-01-09T21:00:00Z', '1970-01-01');
  EXCEPTION WHEN insufficient_privilege THEN RETURN;
  END;
  RAISE EXCEPTION 'expected insufficient_privilege (42501): writer INSERT supplying ingest_ts into risk_event';
END $$;

DO $$ BEGIN
  BEGIN
    INSERT INTO heartbeat (source, event_ts, ingest_ts)
    VALUES ('engine', '2026-01-09T21:00:00Z', '1970-01-01');
  EXCEPTION WHEN insufficient_privilege THEN RETURN;
  END;
  RAISE EXCEPTION 'expected insufficient_privilege (42501): writer INSERT supplying ingest_ts into heartbeat';
END $$;

DO $$ BEGIN
  BEGIN
    UPDATE risk_event SET kind = 'kill_switch';
  EXCEPTION WHEN insufficient_privilege THEN RETURN;
  END;
  RAISE EXCEPTION 'expected insufficient_privilege (42501): UPDATE risk_event';
END $$;

DO $$ BEGIN
  BEGIN
    DELETE FROM heartbeat;
  EXCEPTION WHEN insufficient_privilege THEN RETURN;
  END;
  RAISE EXCEPTION 'expected insufficient_privilege (42501): DELETE FROM heartbeat';
END $$;

RESET ROLE;

-- 8. As the reader: SELECT works on both tables; INSERT is refused (42501).
SET LOCAL ROLE nisse_reader;

DO $$ BEGIN
  PERFORM count(*) FROM risk_event;
  PERFORM count(*) FROM heartbeat;
END $$;

DO $$ BEGIN
  BEGIN
    INSERT INTO risk_event (kind, detail, event_ts)
    VALUES ('kill_switch', '{}', '2026-01-09T21:00:00Z');
  EXCEPTION WHEN insufficient_privilege THEN RETURN;
  END;
  RAISE EXCEPTION 'expected insufficient_privilege (42501): reader INSERT INTO risk_event';
END $$;

DO $$ BEGIN
  BEGIN
    INSERT INTO heartbeat (source, event_ts)
    VALUES ('engine', '2026-01-09T21:00:00Z');
  EXCEPTION WHEN insufficient_privilege THEN RETURN;
  END;
  RAISE EXCEPTION 'expected insufficient_privilege (42501): reader INSERT INTO heartbeat';
END $$;

RESET ROLE;

-- 9. As owner: the writer's rows are there, after this test's own rows, in id order.
SELECT pg_temp.assert_true(
  (SELECT count(*) = 5 FROM risk_event),
  'risk_event must hold exactly the 5 rows inserted by this test');

SELECT pg_temp.assert_true(
  (SELECT count(*) = 3 FROM heartbeat),
  'heartbeat must hold exactly the 3 rows inserted by this test');

SELECT pg_temp.assert_true(
  (SELECT (array_agg(kind ORDER BY id))[5] = 'drawdown_mode' FROM risk_event),
  'the writer''s risk_event row must be the newest row');
