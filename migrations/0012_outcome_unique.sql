-- outcome_window: one outcome per decision and horizon. Two overlapping runs of the nightly scoring
--   job could otherwise both write a row for the same decision; the job already skips decisions
--   that have a row, so this index is the backstop that refuses the second one (23505).
--   No table rows exist in any deployed database yet, so replacing the non-unique index on the same
--   columns (from 0005_evals.sql) is safe. Append-only is unchanged: no UPDATE or DELETE.

CREATE UNIQUE INDEX outcome_window_decision_horizon_key ON outcome_window (decision_id, horizon);
DROP INDEX outcome_window_decision_id_horizon_idx;
