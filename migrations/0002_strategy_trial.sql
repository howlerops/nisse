-- strategy_version: one immutable strategy configuration at one git SHA; parent_id is the
--   version it was derived from.
-- trial_registry: one row per backtest, shadow or live evaluation ever run. The lifetime trial
--   count is the row count and never resets. result is NULL on a row written before the run
--   finishes; rows cannot be updated, so how a later result is attached to such a row is an
--   open design point (see docs/phases/phase-0.md) to settle before Phase 3 writes trials.

CREATE TABLE strategy_version (
  id           BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
  git_sha      TEXT NOT NULL CHECK (git_sha ~ '^[0-9a-f]{40}$'),
  config_hash  BYTEA NOT NULL CHECK (octet_length(config_hash) = 32),
  params       JSONB NOT NULL,
  parent_id    BIGINT NULL REFERENCES strategy_version (id),
  event_ts     TIMESTAMPTZ NOT NULL,
  ingest_ts    TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
  UNIQUE (git_sha, config_hash)
);
COMMENT ON TABLE strategy_version IS 'One immutable strategy configuration at one git SHA.';
SELECT nisse_make_append_only('strategy_version');

CREATE TABLE trial_registry (
  id                   BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
  strategy_version_id  BIGINT NOT NULL REFERENCES strategy_version (id),
  kind                 TEXT NOT NULL CHECK (kind IN ('backtest', 'shadow', 'live')),
  data_range           TSTZRANGE NOT NULL CHECK (NOT isempty(data_range)),
  result               JSONB NULL,
  event_ts             TIMESTAMPTZ NOT NULL,
  ingest_ts            TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp()
);
CREATE INDEX trial_registry_strategy_version_id_idx ON trial_registry (strategy_version_id);
COMMENT ON TABLE trial_registry IS 'One row per backtest, shadow or live evaluation ever run; the row count is the lifetime trial count.';
SELECT nisse_make_append_only('trial_registry');
