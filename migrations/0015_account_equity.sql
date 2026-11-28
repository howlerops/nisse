-- account_equity: the broker account's equity at the close of one session, written by the
--   engine's session-close run. The daily returns the tracking gate compares with a backtest are
--   computed from these rows. The newest row per (environment, strategy config, session_date) is
--   that session's equity: a re-run or a correction is a new row. The strategy config is the
--   strategy_version's config_hash, so a new build of the same config continues the same series.
--   Deposits and withdrawals are not recorded: a session with one has a wrong return.

CREATE TABLE account_equity (
  id                   BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
  strategy_version_id  BIGINT NOT NULL REFERENCES strategy_version (id),
  environment          TEXT NOT NULL CHECK (environment IN ('paper', 'live')),
  session_date         DATE NOT NULL,
  equity               finite_numeric NOT NULL CHECK (equity >= 0),
  event_ts             TIMESTAMPTZ NOT NULL,
  ingest_ts            TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp()
);
CREATE INDEX account_equity_version_session_idx
  ON account_equity (strategy_version_id, session_date, id DESC);
COMMENT ON TABLE account_equity IS 'The broker account''s equity at the close of one session; the newest row per environment, strategy config and session is that session''s equity.';
SELECT nisse_make_append_only('account_equity');

GRANT SELECT ON account_equity TO nisse_reader, nisse_writer;
GRANT INSERT (strategy_version_id, environment, session_date, equity, event_ts)
  ON account_equity TO nisse_writer;

DO $$
DECLARE
  seq text;
BEGIN
  seq := pg_get_serial_sequence('public.account_equity', 'id');
  EXECUTE format('GRANT USAGE ON SEQUENCE %s TO nisse_writer', seq);
END $$;
