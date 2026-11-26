-- risk_event: one row per change of risk state: a new equity high-water mark, a drawdown mode
--   change (or a reset by a person), the kill switch engaged or released, a limits file loaded.
--   The kinds match RiskEventKind in crates/core/src/types.rs. The risk state after a restart is
--   the fold of these rows in id order, which is why they are never updated: a new state is a
--   new row.
-- heartbeat: the engine writes one periodically. A dead-man check (nisse-deadman) on another host
--   reads the newest row for a source, which the index serves, and alerts when it is too old.

CREATE TABLE risk_event (
  id         BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
  kind       TEXT NOT NULL CHECK (kind IN ('equity_peak', 'drawdown_mode', 'kill_switch', 'limits_loaded')),
  detail     JSONB NOT NULL CHECK (jsonb_typeof(detail) = 'object'),
  event_ts   TIMESTAMPTZ NOT NULL,
  ingest_ts  TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp()
);
COMMENT ON TABLE risk_event IS 'A change of risk state (equity peak, drawdown mode, kill switch, limits loaded); the state after a restart is the fold of these rows in id order.';
SELECT nisse_make_append_only('risk_event');

GRANT SELECT ON risk_event TO nisse_reader, nisse_writer;
GRANT INSERT (kind, detail, event_ts) ON risk_event TO nisse_writer;

DO $$
DECLARE
  seq text;
BEGIN
  seq := pg_get_serial_sequence('public.risk_event', 'id');
  EXECUTE format('GRANT USAGE ON SEQUENCE %s TO nisse_writer', seq);
END $$;

CREATE TABLE heartbeat (
  id         BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
  source     TEXT NOT NULL CHECK (source <> ''),
  event_ts   TIMESTAMPTZ NOT NULL,
  ingest_ts  TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp()
);
CREATE INDEX heartbeat_source_event_ts_idx ON heartbeat (source, event_ts DESC);
COMMENT ON TABLE heartbeat IS 'A periodic liveness row written by a process; the newest row per source is read by the dead-man check.';
SELECT nisse_make_append_only('heartbeat');

GRANT SELECT ON heartbeat TO nisse_reader, nisse_writer;
GRANT INSERT (source, event_ts) ON heartbeat TO nisse_writer;

DO $$
DECLARE
  seq text;
BEGIN
  seq := pg_get_serial_sequence('public.heartbeat', 'id');
  EXECUTE format('GRANT USAGE ON SEQUENCE %s TO nisse_writer', seq);
END $$;
