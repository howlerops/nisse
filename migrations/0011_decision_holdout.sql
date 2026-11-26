-- decision_holdout: one row per decision that says whether agents may see it. A decision is hidden
--   when the eval job's keyed hash holds it out (about 25%), and visible otherwise. The assignment
--   is made once: UNIQUE on decision_id allows at most one row per decision, and the table is
--   append-only, so an assignment can never change.
-- agent_decision: the decisions agents may read. It joins decision_holdout (inner join), so a
--   decision with no holdout row yet is NOT visible: unassigned means hidden. Only the decision
--   columns are exposed; `hidden` and `ingest_ts` are not.
--   The view is deliberately NOT security_invoker: it runs with the privileges of its owner, so
--   nisse_agent needs no grant on decision or decision_holdout.
-- nisse_agent: NOLOGIN, the role agents connect as. It can read agent_decision and nothing else.
--   It is not a member of nisse_reader or nisse_writer, and it cannot write any table or view.

CREATE TABLE decision_holdout (
  id           BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
  decision_id  BIGINT NOT NULL UNIQUE REFERENCES decision (id),
  hidden       BOOLEAN NOT NULL,
  event_ts     TIMESTAMPTZ NOT NULL,
  ingest_ts    TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp()
);
COMMENT ON TABLE decision_holdout IS 'Whether a decision is hidden from agents (held out) or visible. Assigned once per decision; a decision with no row is unassigned and therefore hidden from agents.';
SELECT nisse_make_append_only('decision_holdout');

GRANT SELECT ON decision_holdout TO nisse_reader, nisse_writer;
GRANT INSERT (decision_id, hidden, event_ts) ON decision_holdout TO nisse_writer;

DO $$
DECLARE
  seq text;
BEGIN
  seq := pg_get_serial_sequence('public.decision_holdout', 'id');
  EXECUTE format('GRANT USAGE ON SEQUENCE %s TO nisse_writer', seq);
END $$;

-- Created race-safely, as in 0007_roles.sql.
DO $$
DECLARE
  role_name text;
BEGIN
  FOREACH role_name IN ARRAY ARRAY['nisse_agent'] LOOP
    IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = role_name) THEN
      BEGIN
        EXECUTE format('CREATE ROLE %I NOLOGIN', role_name);
      EXCEPTION WHEN duplicate_object OR unique_violation THEN
        NULL;
      END;
    END IF;
  END LOOP;
END $$;

GRANT USAGE ON SCHEMA public TO nisse_agent;

CREATE VIEW agent_decision AS
  SELECT d.id, d.strategy_version_id, d.symbol, d.action, d.intended_qty, d.expected_edge_bps,
         d.expected_vol, d.features, d.feature_hash, d.data_snapshot_id, d.rng_seed,
         d.reason_code, d.event_ts
    FROM decision d
    JOIN decision_holdout h ON h.decision_id = d.id
   WHERE NOT h.hidden;
COMMENT ON VIEW agent_decision IS 'The decisions agents may read: those with a decision_holdout row that is not hidden. A decision with no holdout row is hidden (unassigned means hidden). Runs with the owner''s privileges, deliberately (no security_invoker).';

GRANT SELECT ON agent_decision TO nisse_agent;
