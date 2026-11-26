-- Role and grant checks for 0007_roles.sql. Runs inside the runner's transaction, which is rolled
-- back. The roles are cluster-wide and persist between runs, so the migration must tolerate them.
-- While a restricted role is active, "must fail" checks are DO blocks (they run with the current
-- role's privileges) and "must succeed" checks are plain statements. Assertions that call
-- pg_temp helpers run after RESET ROLE.

-- 1. As owner: both roles exist and cannot log in.
SELECT pg_temp.assert_true(
  (SELECT count(*) = 2 AND bool_and(NOT rolcanlogin)
     FROM pg_roles WHERE rolname IN ('nisse_writer', 'nisse_reader')),
  'nisse_writer and nisse_reader must exist and be NOLOGIN');

-- 2. As the writer: a full valid insert chain succeeds. Each INSERT names its columns and never
--    names id or ingest_ts.
SET LOCAL ROLE nisse_writer;

INSERT INTO strategy_version (git_sha, config_hash, params, event_ts)
VALUES (repeat('a', 40), decode(repeat('ab', 32), 'hex'), '{}', '2026-01-02T00:00:00Z');

INSERT INTO decision (strategy_version_id, symbol, action, intended_qty, expected_edge_bps,
                      expected_vol, features, feature_hash, data_snapshot_id, rng_seed,
                      reason_code, event_ts)
VALUES ((SELECT id FROM strategy_version WHERE git_sha = repeat('a', 40)),
        'SPY', 'enter', 1.5, 12.5, 0.15, '{"mom_12m": 0.1}', decode(repeat('ab', 32), 'hex'),
        'snap-roles', 42, 'trend_up', '2026-01-05T21:00:00Z');

-- Intent row: every column of order_event (0004 and 0006).
INSERT INTO order_event (decision_id, client_order_id, broker_order_id, state, type, limit_px, qty,
                         symbol, side, time_in_force, cum_filled_qty, event_ts)
VALUES ((SELECT id FROM decision WHERE data_snapshot_id = 'snap-roles'), 'nisse-9001', NULL,
        'intent', 'LIMIT', 500.10, 1.5, 'SPY', 'BUY', 'DAY', 0, '2026-01-05T21:00:00Z');

INSERT INTO fill (broker_fill_id, client_order_id, px, qty, fee, event_ts)
VALUES ('exec-roles-1', 'nisse-9001', 500.10, 1.5, 0, '2026-01-05T21:00:02Z');

INSERT INTO market_snapshot (symbol, bid, ask, mid, spread_bps, adv, vol, regime_tag, event_ts)
VALUES ('SPY', 500.10, 500.12, 500.11, 0.4, 1000000, 0.15, 'calm', '2026-01-05T21:00:00Z');

INSERT INTO position_event (symbol, qty_after, avg_cost, event_ts)
VALUES ('SPY', 1.5, 500.10, '2026-01-05T21:00:05Z');

INSERT INTO risk_check (decision_id, rule, limit_value, actual_value, passed, event_ts)
VALUES ((SELECT id FROM decision WHERE data_snapshot_id = 'snap-roles'),
        'max_position_pct', 0.25, 0.10, true, '2026-01-05T21:00:00Z');

INSERT INTO outcome_window (decision_id, horizon, fwd_ret, bench_ret, mfe, mae, event_ts)
VALUES ((SELECT id FROM decision WHERE data_snapshot_id = 'snap-roles'),
        INTERVAL '1 day', 0.012, 0.004, 0.02, -0.01, '2026-02-05T21:00:00Z');

INSERT INTO eval_score (subject_id, subject_type, metric, value, eval_version, event_ts)
VALUES ((SELECT id FROM decision WHERE data_snapshot_id = 'snap-roles'),
        'decision', 'decision_quality', 0.35, 'v1', '2026-02-05T21:00:00Z');

INSERT INTO trial_registry (strategy_version_id, kind, data_range, result, event_ts)
VALUES ((SELECT id FROM strategy_version WHERE git_sha = repeat('a', 40)),
        'backtest', tstzrange('2020-01-01Z', '2025-01-01Z'), NULL, '2026-01-03T00:00:00Z');

-- 3. As the writer, each of these must fail with insufficient_privilege (42501). Postgres checks
--    the privilege before the append-only trigger runs, so UPDATE, DELETE and TRUNCATE give 42501.
DO $$ BEGIN
  BEGIN
    UPDATE decision SET symbol = 'X';
  EXCEPTION WHEN insufficient_privilege THEN RETURN;
  END;
  RAISE EXCEPTION 'expected insufficient_privilege (42501): UPDATE decision';
END $$;

DO $$ BEGIN
  BEGIN
    DELETE FROM fill;
  EXCEPTION WHEN insufficient_privilege THEN RETURN;
  END;
  RAISE EXCEPTION 'expected insufficient_privilege (42501): DELETE FROM fill';
END $$;

DO $$ BEGIN
  BEGIN
    TRUNCATE order_event;
  EXCEPTION WHEN insufficient_privilege THEN RETURN;
  END;
  RAISE EXCEPTION 'expected insufficient_privilege (42501): TRUNCATE order_event';
END $$;

DO $$ BEGIN
  BEGIN
    ALTER TABLE fill DISABLE TRIGGER append_only;
  EXCEPTION WHEN insufficient_privilege THEN RETURN;
  END;
  RAISE EXCEPTION 'expected insufficient_privilege (42501): ALTER TABLE fill DISABLE TRIGGER';
END $$;

DO $$ BEGIN
  BEGIN
    INSERT INTO market_snapshot (symbol, event_ts, ingest_ts)
    VALUES ('SPY', now(), '1970-01-01');
  EXCEPTION WHEN insufficient_privilege THEN RETURN;
  END;
  RAISE EXCEPTION 'expected insufficient_privilege (42501): INSERT supplying ingest_ts';
END $$;

DO $$ BEGIN
  BEGIN
    INSERT INTO schema_migration (version, checksum) VALUES ('9999_x', 'x');
  EXCEPTION WHEN insufficient_privilege THEN RETURN;
  END;
  RAISE EXCEPTION 'expected insufficient_privilege (42501): INSERT INTO schema_migration';
END $$;

DO $$ BEGIN
  BEGIN
    CREATE TABLE sneaky (x int);
  EXCEPTION WHEN insufficient_privilege THEN RETURN;
  END;
  RAISE EXCEPTION 'expected insufficient_privilege (42501): CREATE TABLE';
END $$;

DO $$ BEGIN
  BEGIN
    DROP TABLE fill;
  EXCEPTION WHEN insufficient_privilege THEN RETURN;
  END;
  RAISE EXCEPTION 'expected insufficient_privilege (42501): DROP TABLE fill';
END $$;

-- 4. As owner: the database set ingest_ts on the writer's rows, and the rows are there.
RESET ROLE;

SELECT pg_temp.assert_true(
  (SELECT ingest_ts IS NOT NULL AND ingest_ts > '2020-01-01Z'
     FROM market_snapshot WHERE symbol = 'SPY'),
  'ingest_ts of a writer-inserted market_snapshot must be set by the database');

SELECT pg_temp.assert_true(
  (SELECT count(*) = 1 FROM fill WHERE broker_fill_id = 'exec-roles-1'),
  'the writer''s fill row must be present');

SELECT pg_temp.assert_true(
  (SELECT count(*) = 1 FROM decision WHERE data_snapshot_id = 'snap-roles'),
  'the writer''s decision row must be present');

-- 5. As the reader: SELECT works, INSERT fails with 42501.
SET LOCAL ROLE nisse_reader;

DO $$ BEGIN
  PERFORM count(*) FROM decision;
END $$;

DO $$ BEGIN
  BEGIN
    INSERT INTO market_snapshot (symbol, event_ts) VALUES ('SPY', now());
  EXCEPTION WHEN insufficient_privilege THEN RETURN;
  END;
  RAISE EXCEPTION 'expected insufficient_privilege (42501): reader INSERT INTO market_snapshot';
END $$;

RESET ROLE;

-- 6. As owner, from the catalog: for every store table (any future table is included), the writer
--    and the reader can SELECT; the writer has column INSERT but not on id or ingest_ts; neither
--    role has UPDATE, DELETE, TRUNCATE, REFERENCES or TRIGGER on it.
DO $$
DECLARE
  tbl  text;
  who  text;
  priv text;
BEGIN
  FOR tbl IN
    SELECT tablename::text FROM pg_tables
     WHERE schemaname = 'public' AND tablename <> 'schema_migration'
  LOOP
    PERFORM pg_temp.assert_true(
      has_table_privilege('nisse_reader', format('public.%I', tbl), 'SELECT'),
      format('nisse_reader must have SELECT on %s (grant it in its migration)', tbl));
    PERFORM pg_temp.assert_true(
      has_table_privilege('nisse_writer', format('public.%I', tbl), 'SELECT'),
      format('nisse_writer must have SELECT on %s', tbl));
    PERFORM pg_temp.assert_true(
      has_any_column_privilege('nisse_writer', format('public.%I', tbl), 'INSERT'),
      format('nisse_writer must have column INSERT on %s', tbl));
    PERFORM pg_temp.assert_true(
      NOT has_column_privilege('nisse_writer', format('public.%I', tbl), 'ingest_ts', 'INSERT'),
      format('nisse_writer must not INSERT ingest_ts on %s', tbl));
    PERFORM pg_temp.assert_true(
      NOT has_column_privilege('nisse_writer', format('public.%I', tbl), 'id', 'INSERT'),
      format('nisse_writer must not INSERT id on %s', tbl));

    FOREACH who IN ARRAY ARRAY['nisse_writer', 'nisse_reader'] LOOP
      FOREACH priv IN ARRAY ARRAY['UPDATE', 'DELETE', 'TRUNCATE', 'REFERENCES', 'TRIGGER'] LOOP
        PERFORM pg_temp.assert_true(
          NOT has_table_privilege(who, format('public.%I', tbl), priv),
          format('%s must not have %s on %s', who, priv, tbl));
      END LOOP;
    END LOOP;
  END LOOP;
END $$;

-- 6b. schema_migration: both roles may SELECT it and nothing else; neither may CREATE in public.
SELECT pg_temp.assert_true(
  has_table_privilege('nisse_reader', 'schema_migration', 'SELECT')
  AND has_table_privilege('nisse_writer', 'schema_migration', 'SELECT'),
  'both roles must SELECT schema_migration');

SELECT pg_temp.assert_true(
  NOT (has_table_privilege('nisse_writer', 'schema_migration', 'INSERT')
    OR has_any_column_privilege('nisse_writer', 'schema_migration', 'INSERT')
    OR has_table_privilege('nisse_writer', 'schema_migration', 'UPDATE')
    OR has_table_privilege('nisse_writer', 'schema_migration', 'DELETE')
    OR has_table_privilege('nisse_writer', 'schema_migration', 'TRUNCATE')
    OR has_table_privilege('nisse_reader', 'schema_migration', 'INSERT')
    OR has_table_privilege('nisse_reader', 'schema_migration', 'UPDATE')
    OR has_table_privilege('nisse_reader', 'schema_migration', 'DELETE')
    OR has_table_privilege('nisse_reader', 'schema_migration', 'TRUNCATE')),
  'neither role may write schema_migration');

SELECT pg_temp.assert_true(
  NOT has_schema_privilege('nisse_writer', 'public', 'CREATE')
  AND NOT has_schema_privilege('nisse_reader', 'public', 'CREATE'),
  'neither role may CREATE in schema public');
