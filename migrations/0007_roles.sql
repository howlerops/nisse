-- Database roles for the store. nisse_writer is what the engine connects as: it can INSERT every
--   column of a store table except `id` and `ingest_ts`, so it cannot choose a row id or backdate
--   the receive time. It does not own the tables, so it cannot disable the append-only trigger.
--   nisse_reader can only SELECT (research and eval jobs).
-- Both roles are NOLOGIN group roles. Roles are cluster-wide, so they are created only if missing;
--   a login user is created and granted membership of one of them at deploy time (Phase 6).
-- Grants are explicit per table. A new store table needs its own grants in its own migration, so
--   there is no GRANT ... ON ALL TABLES and no ALTER DEFAULT PRIVILEGES.
-- Nothing here grants UPDATE, DELETE, TRUNCATE, REFERENCES or TRIGGER (migrations/tests/0008_roles.sql).

-- Two databases on one server can be migrated at the same moment (parallel test databases do
-- exactly that), so "create if missing" must also survive losing that race.
DO $$
DECLARE
  role_name text;
BEGIN
  FOREACH role_name IN ARRAY ARRAY['nisse_writer', 'nisse_reader'] LOOP
    IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = role_name) THEN
      BEGIN
        EXECUTE format('CREATE ROLE %I NOLOGIN', role_name);
      EXCEPTION WHEN duplicate_object OR unique_violation THEN
        NULL;
      END;
    END IF;
  END LOOP;
END $$;

GRANT USAGE ON SCHEMA public TO nisse_writer, nisse_reader;

DO $$
DECLARE
  tbl  text;
  cols text;
  seq  text;
BEGIN
  FOREACH tbl IN ARRAY ARRAY[
    'strategy_version', 'trial_registry', 'decision', 'market_snapshot', 'risk_check',
    'order_event', 'fill', 'position_event', 'outcome_window', 'eval_score'
  ] LOOP
    SELECT string_agg(quote_ident(column_name), ', ' ORDER BY ordinal_position)
      INTO cols
      FROM information_schema.columns
     WHERE table_schema = 'public'
       AND table_name = tbl
       AND column_name NOT IN ('id', 'ingest_ts');

    seq := pg_get_serial_sequence(format('public.%I', tbl), 'id');

    EXECUTE format('GRANT SELECT ON public.%I TO nisse_reader, nisse_writer', tbl);
    EXECUTE format('GRANT INSERT (%s) ON public.%I TO nisse_writer', cols, tbl);
    EXECUTE format('GRANT USAGE ON SEQUENCE %s TO nisse_writer', seq);
  END LOOP;
END $$;

GRANT SELECT ON schema_migration TO nisse_reader, nisse_writer;
