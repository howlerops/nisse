-- outcome_window: what happened after a decision over one fixed horizon (forward return, benchmark
--   return, maximum favourable and adverse excursion). Horizons are fixed before the trade. Rows
--   exist for skips and holds too.
-- eval_score: one metric value for one subject (a decision or a strategy version), tagged with the
--   eval_version of the code that computed it. A recomputation is a new row; readers take the
--   latest row per (subject, metric, eval_version). subject_id is the id of a row in the table
--   named by subject_type, so it has no foreign key.
-- These columns are returns and statistics, not money, so they are DOUBLE PRECISION.

CREATE TABLE outcome_window (
  id           BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
  decision_id  BIGINT NOT NULL REFERENCES decision (id),
  horizon      INTERVAL NOT NULL             CHECK (horizon > INTERVAL '0'),
  fwd_ret      DOUBLE PRECISION NOT NULL,
  bench_ret    DOUBLE PRECISION NOT NULL,
  mfe          DOUBLE PRECISION NOT NULL,
  mae          DOUBLE PRECISION NOT NULL,
  event_ts     TIMESTAMPTZ NOT NULL,
  ingest_ts    TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp()
);
CREATE INDEX outcome_window_decision_id_horizon_idx ON outcome_window (decision_id, horizon);
COMMENT ON TABLE outcome_window IS 'The forward return, benchmark return and excursions after a decision over one fixed horizon.';
SELECT nisse_make_append_only('outcome_window');

CREATE TABLE eval_score (
  id            BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
  subject_id    BIGINT NOT NULL,
  subject_type  TEXT NOT NULL                CHECK (subject_type IN ('decision', 'strategy_version')),
  metric        TEXT NOT NULL                CHECK (metric <> ''),
  value         DOUBLE PRECISION NOT NULL,
  eval_version  TEXT NOT NULL                CHECK (eval_version <> ''),
  event_ts      TIMESTAMPTZ NOT NULL,
  ingest_ts     TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp()
);
CREATE INDEX eval_score_subject_idx ON eval_score (subject_type, subject_id, metric);
COMMENT ON TABLE eval_score IS 'One metric value for a decision or strategy version, tagged with the eval_version that computed it.';
SELECT nisse_make_append_only('eval_score');
