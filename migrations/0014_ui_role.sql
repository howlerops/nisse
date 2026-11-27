-- The phone UI's database role and the one function it may call.
--   A phone can be lost, so its credential must be able to do only two things: read the store
--   (through nisse_reader) and engage the kill switch, which stops trading. It cannot release the
--   switch, cannot write any table, and cannot touch the append-only guards. Releasing the switch
--   is a decision a person makes on the engine host, so there is no function for it here.
-- nisse_ui: NOLOGIN, like the other roles. The owner creates a login role that is a member of it
--   (at deploy time, as for nisse_writer). It is a member of nisse_reader, so it can SELECT the
--   store and nothing more. It has no INSERT, UPDATE or DELETE on any table.
-- nisse_engage_kill_switch(by): the only write it may make. It inserts one risk_event row of kind
--   kill_switch with the same detail shape the engine writes ({"engaged": true, "by": ...}) plus
--   "via": "ui", and returns the new row's id. It is SECURITY DEFINER so that nisse_ui does not
--   need INSERT on risk_event; the search_path is pinned so a caller cannot redirect the table
--   reference. EXECUTE is revoked from PUBLIC (PostgreSQL grants it to PUBLIC by default) and
--   granted to nisse_ui only. nisse_writer cannot call it: that role writes risk events through
--   its own INSERT grant (0010_risk_event.sql), not through this function.
-- Nothing here grants UPDATE, DELETE or TRUNCATE, and there is no new table (see
--   migrations/tests/0015_ui_role.sql).

-- Created race-safely, as in 0007_roles.sql and 0011_decision_holdout.sql.
DO $$
DECLARE
  role_name text;
BEGIN
  FOREACH role_name IN ARRAY ARRAY['nisse_ui'] LOOP
    IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = role_name) THEN
      BEGIN
        EXECUTE format('CREATE ROLE %I NOLOGIN', role_name);
      EXCEPTION WHEN duplicate_object OR unique_violation THEN
        NULL;
      END;
    END IF;
  END LOOP;
END $$;

GRANT nisse_reader TO nisse_ui;
GRANT USAGE ON SCHEMA public TO nisse_ui;

-- Rejects a missing or blank name, and one longer than 64 characters after trimming, with
-- SQLSTATE 22023 (invalid_parameter_value); nothing is inserted in that case. The name is trimmed
-- before the length check, and the trimmed name is what is stored.
CREATE FUNCTION nisse_engage_kill_switch(by text) RETURNS bigint
  LANGUAGE plpgsql SECURITY DEFINER
  SET search_path = pg_catalog, public
AS $$
DECLARE
  who    text;
  new_id bigint;
BEGIN
  IF by IS NULL THEN
    RAISE EXCEPTION 'kill switch: "by" must be given' USING ERRCODE = '22023';
  END IF;

  who := btrim(by);
  IF who = '' OR char_length(who) > 64 THEN
    RAISE EXCEPTION 'kill switch: "by" must be 1 to 64 characters' USING ERRCODE = '22023';
  END IF;

  INSERT INTO public.risk_event (kind, detail, event_ts)
  VALUES ('kill_switch',
          jsonb_build_object('engaged', true, 'by', who, 'via', 'ui'),
          clock_timestamp())
  RETURNING id INTO new_id;

  RETURN new_id;
END;
$$;

REVOKE ALL ON FUNCTION nisse_engage_kill_switch(text) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION nisse_engage_kill_switch(text) TO nisse_ui;

COMMENT ON FUNCTION nisse_engage_kill_switch(text) IS 'Engages the kill switch for the named person (1 to 64 characters, trimmed) by inserting one kill_switch risk_event tagged via ui, and returns its id; it cannot release the switch. Runs with the owner''s privileges, deliberately (SECURITY DEFINER, pinned search_path).';
