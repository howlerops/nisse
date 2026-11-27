-- Role and function checks for nisse_ui and nisse_engage_kill_switch (0014_ui_role.sql). Runs
-- inside the runner's transaction, which is rolled back. While a restricted role is active,
-- "must work" and "must fail" checks are DO blocks that run as that role; assertions that call
-- pg_temp helpers run after RESET ROLE.

-- 1. As owner: nisse_ui exists, cannot log in, and is a member of nisse_reader.
SELECT pg_temp.assert_true(
  (SELECT count(*) = 1 AND bool_and(NOT rolcanlogin) FROM pg_roles WHERE rolname = 'nisse_ui'),
  'nisse_ui must exist and be NOLOGIN');

SELECT pg_temp.assert_true(
  pg_has_role('nisse_ui', 'nisse_reader', 'MEMBER'),
  'nisse_ui must be a member of nisse_reader');

-- 2. As owner: the function is SECURITY DEFINER with a pinned search_path.
SELECT pg_temp.assert_true(
  (SELECT count(*) = 1
          AND bool_and(prosecdef)
          AND bool_and(EXISTS (SELECT 1 FROM unnest(proconfig) AS c WHERE c LIKE 'search_path=%'))
     FROM pg_proc
    WHERE pronamespace = 'public'::regnamespace AND proname = 'nisse_engage_kill_switch'),
  'nisse_engage_kill_switch must be SECURITY DEFINER with a pinned search_path');

-- 3. As owner: no function in public whose name contains "release".
SELECT pg_temp.assert_true(
  NOT EXISTS (SELECT 1 FROM pg_proc
               WHERE pronamespace = 'public'::regnamespace AND proname LIKE '%release%'),
  'there must be no release function in schema public');

-- 4. As owner: EXECUTE is granted to nisse_ui only, not to the other roles.
SELECT pg_temp.assert_true(
  has_function_privilege('nisse_ui', 'nisse_engage_kill_switch(text)', 'EXECUTE')
  AND NOT has_function_privilege('nisse_agent', 'nisse_engage_kill_switch(text)', 'EXECUTE')
  AND NOT has_function_privilege('nisse_reader', 'nisse_engage_kill_switch(text)', 'EXECUTE')
  AND NOT has_function_privilege('nisse_writer', 'nisse_engage_kill_switch(text)', 'EXECUTE'),
  'EXECUTE on nisse_engage_kill_switch must be granted to nisse_ui only');

-- 5. As nisse_ui: the call returns the id of the row it inserted, which is the newest kill_switch
--    row, with engaged, by and via set. Surrounding spaces in "by" are trimmed.
SET LOCAL ROLE nisse_ui;

DO $$
DECLARE
  got       bigint;
  newest    bigint;
  engaged_v text;
  by_v      text;
  via_v     text;
BEGIN
  got := nisse_engage_kill_switch('ana');

  SELECT id, detail->>'engaged', detail->>'by', detail->>'via'
    INTO newest, engaged_v, by_v, via_v
    FROM risk_event WHERE kind = 'kill_switch' ORDER BY id DESC LIMIT 1;

  IF got IS NULL OR got <> newest THEN
    RAISE EXCEPTION 'assertion failed: call must return the new row id (got %, newest %)', got, newest;
  END IF;
  IF engaged_v IS DISTINCT FROM 'true' OR by_v IS DISTINCT FROM 'ana' OR via_v IS DISTINCT FROM 'ui' THEN
    RAISE EXCEPTION 'assertion failed: detail must be engaged=true, by=ana, via=ui (got %, %, %)',
      engaged_v, by_v, via_v;
  END IF;

  got := nisse_engage_kill_switch('  bo  ');

  SELECT id, detail->>'by' INTO newest, by_v
    FROM risk_event WHERE kind = 'kill_switch' ORDER BY id DESC LIMIT 1;

  IF got IS NULL OR got <> newest THEN
    RAISE EXCEPTION 'assertion failed: trimmed call must return the new row id';
  END IF;
  IF by_v IS DISTINCT FROM 'bo' THEN
    RAISE EXCEPTION 'assertion failed: "by" must be trimmed (got %)', by_v;
  END IF;
END $$;

-- 6. As nisse_ui: NULL, empty, blank and 65-character names are refused with 22023, and nothing
--    is inserted.
DO $$
DECLARE
  arg    text;
  before bigint;
  after  bigint;
BEGIN
  SELECT count(*) INTO before FROM risk_event WHERE kind = 'kill_switch';

  FOREACH arg IN ARRAY ARRAY[NULL::text, '', '   ', repeat('a', 65)] LOOP
    BEGIN
      PERFORM nisse_engage_kill_switch(arg);
    EXCEPTION WHEN SQLSTATE '22023' THEN
      CONTINUE;
    END;
    RAISE EXCEPTION 'expected SQLSTATE 22023 for "by" = %', coalesce(arg, 'NULL');
  END LOOP;

  SELECT count(*) INTO after FROM risk_event WHERE kind = 'kill_switch';
  IF after <> before THEN
    RAISE EXCEPTION 'assertion failed: refused calls must insert nothing (% before, % after)',
      before, after;
  END IF;
END $$;

-- 7. As nisse_ui: a 64-character name works, and so does one that is 64 characters once trimmed.
DO $$
DECLARE
  got   bigint;
  by_v  text;
BEGIN
  got := nisse_engage_kill_switch(repeat('b', 64));
  SELECT detail->>'by' INTO by_v FROM risk_event WHERE id = got;
  IF by_v IS DISTINCT FROM repeat('b', 64) THEN
    RAISE EXCEPTION 'assertion failed: a 64-character name must be stored whole';
  END IF;

  got := nisse_engage_kill_switch(' ' || repeat('c', 64) || ' ');
  SELECT detail->>'by' INTO by_v FROM risk_event WHERE id = got;
  IF by_v IS DISTINCT FROM repeat('c', 64) THEN
    RAISE EXCEPTION 'assertion failed: 64 characters after trimming must be accepted';
  END IF;
END $$;

-- 8. As nisse_ui: SELECT works on the tables it reads (reader membership).
DO $$
BEGIN
  PERFORM count(*) FROM risk_event;
  PERFORM count(*) FROM position_event;
  PERFORM count(*) FROM heartbeat;
END $$;

-- 9. As nisse_ui: every direct write is refused (42501). It writes only through the function.
DO $$
DECLARE
  stmt text;
BEGIN
  FOREACH stmt IN ARRAY ARRAY[
    'INSERT INTO risk_event (kind, detail, event_ts) VALUES (''kill_switch'', ''{}''::jsonb, now())',
    'INSERT INTO heartbeat (source, event_ts) VALUES (''ui'', now())',
    'UPDATE risk_event SET detail = detail',
    'DELETE FROM risk_event'
  ] LOOP
    BEGIN
      EXECUTE stmt;
    EXCEPTION WHEN insufficient_privilege THEN
      CONTINUE;
    END;
    RAISE EXCEPTION 'expected insufficient_privilege (42501): nisse_ui ran %', stmt;
  END LOOP;
END $$;

RESET ROLE;

-- 10. Other roles cannot call the function (42501). nisse_agent is not in nisse_ui, and the
--     writer writes risk events through its own INSERT grant, not through this function.
SET LOCAL ROLE nisse_agent;

DO $$
BEGIN
  BEGIN
    PERFORM nisse_engage_kill_switch('mallory');
  EXCEPTION WHEN insufficient_privilege THEN
    RETURN;
  END;
  RAISE EXCEPTION 'expected insufficient_privilege (42501): nisse_agent called the function';
END $$;

RESET ROLE;

SET LOCAL ROLE nisse_reader;

DO $$
BEGIN
  BEGIN
    PERFORM nisse_engage_kill_switch('mallory');
  EXCEPTION WHEN insufficient_privilege THEN
    RETURN;
  END;
  RAISE EXCEPTION 'expected insufficient_privilege (42501): nisse_reader called the function';
END $$;

RESET ROLE;

SET LOCAL ROLE nisse_writer;

DO $$
BEGIN
  BEGIN
    PERFORM nisse_engage_kill_switch('mallory');
  EXCEPTION WHEN insufficient_privilege THEN
    RETURN;
  END;
  RAISE EXCEPTION 'expected insufficient_privilege (42501): nisse_writer called the function';
END $$;

RESET ROLE;

-- 11. As owner, after the calls above: the four accepted calls ('ana', '  bo  ', and the two
--     64-character names) left exactly four kill_switch rows, each tagged via ui and engaged.
--     The refused calls left none.
SELECT pg_temp.assert_true(
  (SELECT count(*) = 4
     FROM risk_event
    WHERE kind = 'kill_switch' AND detail->>'via' = 'ui' AND detail->>'engaged' = 'true'),
  'the accepted calls must leave exactly four ui kill_switch rows, and the refused ones none');
