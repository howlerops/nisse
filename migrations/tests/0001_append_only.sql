-- Contract checks that hold for every table in the store, present and future.

-- The number domains reject every non-finite value and keep exact decimals.
CREATE TEMP TABLE finite_probe (n finite_numeric, d finite_double) ON COMMIT DROP;
INSERT INTO finite_probe VALUES (0.1, 0.1), (-1, -1), (NULL, NULL);
SELECT pg_temp.assert_true((SELECT sum(n) = -0.9 FROM finite_probe), 'finite_numeric is exact');
SELECT pg_temp.assert_raises(
  format('INSERT INTO finite_probe (%s) VALUES (%L)', col, bad), '23514')
FROM unnest(ARRAY['n', 'd']) AS col, unnest(ARRAY['NaN', 'Infinity', '-Infinity']) AS bad;

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

    PERFORM pg_temp.assert_true(
      NOT EXISTS (
        SELECT 1
        FROM pg_attribute a
        WHERE a.attrelid = t.oid
          AND a.attnum > 0
          AND NOT a.attisdropped
          AND a.atttypid IN ('numeric'::regtype, 'float8'::regtype, 'float4'::regtype)),
      t.relname || ' has a bare NUMERIC or float column; use finite_numeric / finite_double');

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
