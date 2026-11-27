-- engine_step: one row per status change of one step of one scheduled engine run (for example
--   the session-close step "rebalance" of run "session-close/2026-01-05"). A step starts as
--   'started', then becomes 'done' or 'failed'. The newest row per (run_key, step) is the step's
--   status, which the index serves. A re-run of the same run key skips the steps already 'done'.
--   Rows are never updated: a new status is a new row, so the history of a run is kept.

CREATE TABLE engine_step (
  id         BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
  run_key    TEXT NOT NULL CHECK (run_key <> ''),
  step       TEXT NOT NULL CHECK (step <> ''),
  status     TEXT NOT NULL CHECK (status IN ('started', 'done', 'failed')),
  detail     JSONB NOT NULL CHECK (jsonb_typeof(detail) = 'object'),
  event_ts   TIMESTAMPTZ NOT NULL,
  ingest_ts  TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp()
);
CREATE INDEX engine_step_run_key_step_idx ON engine_step (run_key, step, id DESC);
COMMENT ON TABLE engine_step IS 'One row per status change of one step of one scheduled engine run; the newest row per (run_key, step) is the step''s status, and a re-run skips steps already done.';
SELECT nisse_make_append_only('engine_step');

GRANT SELECT ON engine_step TO nisse_reader, nisse_writer;
GRANT INSERT (run_key, step, status, detail, event_ts) ON engine_step TO nisse_writer;

DO $$
DECLARE
  seq text;
BEGIN
  seq := pg_get_serial_sequence('public.engine_step', 'id');
  EXECUTE format('GRANT USAGE ON SEQUENCE %s TO nisse_writer', seq);
END $$;
