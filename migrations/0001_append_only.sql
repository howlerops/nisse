-- Append-only guard shared by every store table.
--
-- Store tables only ever take INSERTs: a correction is a new row. Each table migration ends
-- with `SELECT nisse_make_append_only('<table>');`, which attaches one statement-level trigger
-- that rejects UPDATE, DELETE and TRUNCATE (even when no row would match).
--
-- Column contract for every store table (checked by migrations/tests/0001_append_only.sql):
--   id        BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY
--   event_ts  TIMESTAMPTZ NOT NULL                            -- when it happened; the writer
--                                                             -- supplies it from its clock
--   ingest_ts TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp()  -- when the store received it

CREATE FUNCTION nisse_forbid_mutation() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
  RAISE EXCEPTION '% is not allowed on append-only table %; insert a correcting row instead',
    TG_OP, TG_TABLE_NAME
    USING ERRCODE = 'restrict_violation';
END;
$$;

CREATE FUNCTION nisse_make_append_only(tbl regclass) RETURNS void
LANGUAGE plpgsql AS $$
BEGIN
  EXECUTE format(
    'CREATE TRIGGER append_only BEFORE UPDATE OR DELETE OR TRUNCATE ON %s
       FOR EACH STATEMENT EXECUTE FUNCTION nisse_forbid_mutation()',
    tbl);
END;
$$;

-- The migration ledger (created by scripts/migrate.sh) follows the same rule.
SELECT nisse_make_append_only('schema_migration');
