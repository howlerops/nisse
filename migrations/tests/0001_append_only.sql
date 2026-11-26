-- Contract checks that hold for every table in the store, present and future.

SELECT pg_temp.assert_append_only('schema_migration');

DO $$
DECLARE
  t record;
BEGIN
  FOR t IN
    SELECT c.oid, c.relname
    FROM pg_class c
    WHERE c.relnamespace = 'public'::regnamespace AND c.relkind IN ('r', 'p')
  LOOP
    PERFORM pg_temp.assert_true(
      (SELECT count(*) = 2
       FROM pg_attribute a
       WHERE a.attrelid = t.oid
         AND a.attname IN ('event_ts', 'ingest_ts')
         AND a.atttypid = 'timestamptz'::regtype
         AND a.attnotnull),
      t.relname || ' needs event_ts and ingest_ts as TIMESTAMPTZ NOT NULL');

    PERFORM pg_temp.assert_true(
      (SELECT a.atthasdef
       FROM pg_attribute a
       WHERE a.attrelid = t.oid AND a.attname = 'ingest_ts'),
      t.relname || '.ingest_ts needs a default');

    PERFORM pg_temp.assert_true(
      EXISTS (
        SELECT 1
        FROM pg_trigger g
        WHERE g.tgrelid = t.oid
          AND g.tgname = 'append_only'
          AND g.tgenabled = 'O'
          AND g.tgfoid = 'nisse_forbid_mutation'::regproc),
      t.relname || ' is missing the append_only trigger (call nisse_make_append_only)');

    PERFORM pg_temp.assert_append_only(t.oid::regclass);

    IF t.relname <> 'schema_migration' THEN
      PERFORM pg_temp.assert_true(
        (SELECT a.attidentity = 'a' AND a.atttypid = 'bigint'::regtype
         FROM pg_attribute a
         WHERE a.attrelid = t.oid AND a.attname = 'id'),
        t.relname || '.id must be BIGINT GENERATED ALWAYS AS IDENTITY');
    END IF;
  END LOOP;
END;
$$;
