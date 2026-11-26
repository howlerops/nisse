-- trial_result: the outcome of one trial, written when its run finishes.
--   A trial is registered in trial_registry BEFORE it runs, so a run that crashes still counts
--   toward the lifetime trial budget. Its outcome is written here when it finishes. Rows cannot
--   be updated, which is why the result is not a column of trial_registry. At most one result
--   per trial (the UNIQUE constraint), so the trial count is count(*) of trial_registry and
--   never of this table. trial_registry.result remains for trials whose result was known when
--   they were registered.

CREATE TABLE trial_result (
  id         BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
  trial_id   BIGINT NOT NULL UNIQUE REFERENCES trial_registry (id),
  result     JSONB NOT NULL,
  event_ts   TIMESTAMPTZ NOT NULL,
  ingest_ts  TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp()
);
COMMENT ON TABLE trial_result IS 'The outcome of one trial, written when its run finishes; at most one per trial.';
SELECT nisse_make_append_only('trial_result');

GRANT SELECT ON trial_result TO nisse_reader, nisse_writer;
GRANT INSERT (trial_id, result, event_ts) ON trial_result TO nisse_writer;

DO $$
DECLARE
  seq text;
BEGIN
  seq := pg_get_serial_sequence('public.trial_result', 'id');
  EXECUTE format('GRANT USAGE ON SEQUENCE %s TO nisse_writer', seq);
END $$;
