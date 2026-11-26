-- decision: one row per strategy decision per symbol, including holds and skips, with everything
--   needed to replay it (features, feature_hash, data_snapshot_id, rng_seed; the config hash and
--   git SHA come from strategy_version).
-- market_snapshot: the quote and market state seen at a point in time.
-- risk_check: one row per risk rule evaluated for a decision, pass or fail.
-- Money and quantity columns are NUMERIC (exact); ratios and statistics are DOUBLE PRECISION.

CREATE TABLE decision (
  id                   BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
  strategy_version_id  BIGINT NOT NULL REFERENCES strategy_version (id),
  symbol               TEXT NOT NULL             CHECK (symbol <> ''),
  action               TEXT NOT NULL             CHECK (action IN ('enter', 'exit', 'hold', 'skip')),
  intended_qty         NUMERIC NOT NULL,
  expected_edge_bps    DOUBLE PRECISION NULL,
  expected_vol         DOUBLE PRECISION NULL,
  features             JSONB NOT NULL,
  feature_hash         BYTEA NOT NULL            CHECK (octet_length(feature_hash) = 32),
  data_snapshot_id     TEXT NOT NULL,
  rng_seed             BIGINT NOT NULL,
  reason_code          TEXT NOT NULL             CHECK (reason_code <> ''),
  event_ts             TIMESTAMPTZ NOT NULL,
  ingest_ts            TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp()
);
CREATE INDEX decision_strategy_version_id_event_ts_idx ON decision (strategy_version_id, event_ts);
COMMENT ON TABLE decision IS 'One strategy decision per symbol, including holds and skips, with what is needed to replay it.';
SELECT nisse_make_append_only('decision');

CREATE TABLE market_snapshot (
  id          BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
  symbol      TEXT NOT NULL                      CHECK (symbol <> ''),
  bid         NUMERIC NULL,
  ask         NUMERIC NULL,
  mid         NUMERIC NULL,
  spread_bps  DOUBLE PRECISION NULL,
  adv         NUMERIC NULL,
  vol         DOUBLE PRECISION NULL,
  regime_tag  TEXT NULL,
  event_ts    TIMESTAMPTZ NOT NULL,
  ingest_ts   TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
  CHECK (bid <= ask)  -- passes when either side is NULL (one-sided or missing quote)
);
CREATE INDEX market_snapshot_symbol_event_ts_idx ON market_snapshot (symbol, event_ts);
COMMENT ON TABLE market_snapshot IS 'The quote and market state seen for a symbol at a point in time.';
SELECT nisse_make_append_only('market_snapshot');

CREATE TABLE risk_check (
  id            BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
  decision_id   BIGINT NOT NULL REFERENCES decision (id),
  rule          TEXT NOT NULL                    CHECK (rule <> ''),
  limit_value   NUMERIC NOT NULL,
  actual_value  NUMERIC NOT NULL,
  passed        BOOLEAN NOT NULL,
  event_ts      TIMESTAMPTZ NOT NULL,
  ingest_ts     TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp()
);
CREATE INDEX risk_check_decision_id_idx ON risk_check (decision_id);
COMMENT ON TABLE risk_check IS 'One risk rule evaluated for a decision, with its pass or fail result.';
SELECT nisse_make_append_only('risk_check');
