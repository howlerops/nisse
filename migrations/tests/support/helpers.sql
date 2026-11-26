-- Assertion helpers for migrations/tests/*.sql. Loaded by scripts/test-migrations.sh before
-- each test file, inside the transaction that the runner rolls back.


-- TRUNCATE ... CASCADE in assert_append_only announces each cascade; keep test output quiet.
SET LOCAL client_min_messages = warning;
-- Fails the test unless `cond` is true (NULL counts as false).
CREATE FUNCTION pg_temp.assert_true(cond boolean, msg text) RETURNS void
LANGUAGE plpgsql AS $$
BEGIN
  IF cond IS NOT TRUE THEN
    RAISE EXCEPTION 'assertion failed: %', msg;
  END IF;
END;
$$;

-- Fails the test unless running `stmt` raises an error with SQLSTATE `want_state`.
-- The statement's effects are rolled back either way. Common states:
--   23001 append-only guard    23502 NOT NULL    23503 foreign key
--   23505 unique               23514 CHECK
CREATE FUNCTION pg_temp.assert_raises(stmt text, want_state text) RETURNS void
LANGUAGE plpgsql AS $$
BEGIN
  BEGIN
    EXECUTE stmt;
  EXCEPTION WHEN OTHERS THEN
    IF SQLSTATE = want_state THEN
      RETURN;
    END IF;
    RAISE EXCEPTION 'expected SQLSTATE % but got % (%) from: %',
      want_state, SQLSTATE, SQLERRM, stmt;
  END;
  RAISE EXCEPTION 'expected SQLSTATE % but statement succeeded: %', want_state, stmt;
END;
$$;

-- Fails the test unless UPDATE, DELETE and TRUNCATE are all rejected on `tbl`.
CREATE FUNCTION pg_temp.assert_append_only(tbl regclass) RETURNS void
LANGUAGE plpgsql AS $$
BEGIN
  PERFORM pg_temp.assert_raises(format('UPDATE %s SET ingest_ts = ingest_ts', tbl), '23001');
  PERFORM pg_temp.assert_raises(format('DELETE FROM %s', tbl), '23001');
  -- CASCADE, because a plain TRUNCATE of a table that foreign keys point at is refused by
  -- Postgres (0A000) before any trigger runs; the cascading form is the one the guard must stop.
  PERFORM pg_temp.assert_raises(format('TRUNCATE %s CASCADE', tbl), '23001');
END;
$$;
