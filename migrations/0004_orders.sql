-- order_event: one row per state change of an order. The first row for a client_order_id is the
--   write-ahead intent, inserted BEFORE the order is sent to the broker, so broker_order_id is NULL
--   on it and filled in on later rows. The list of state values is fixed in Phase 1 with the adapter.
-- fill: one row per fill reported by the broker, keyed to its order by client_order_id. qty is the
--   quantity of that one fill; cumulative filled quantity is the sum over the order's fills. fee may
--   be negative (rebate).
-- position_event: the position in a symbol after a change. qty_after may be negative (short).
-- Prices, quantities and fees are NUMERIC (exact), never floating point.

CREATE TABLE order_event (
  id               BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
  decision_id      BIGINT NOT NULL REFERENCES decision (id),
  client_order_id  TEXT NOT NULL             CHECK (client_order_id <> ''),
  broker_order_id  TEXT NULL,
  state            TEXT NOT NULL             CHECK (state <> ''),
  type             TEXT NOT NULL             CHECK (type <> ''),
  limit_px         NUMERIC NULL              CHECK (limit_px > 0),
  qty              NUMERIC NOT NULL          CHECK (qty > 0),
  event_ts         TIMESTAMPTZ NOT NULL,
  ingest_ts        TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp()
);
CREATE INDEX order_event_client_order_id_event_ts_idx ON order_event (client_order_id, event_ts);
CREATE INDEX order_event_decision_id_idx ON order_event (decision_id);
COMMENT ON TABLE order_event IS 'One row per state change of an order; the first row for a client_order_id is the write-ahead intent.';
SELECT nisse_make_append_only('order_event');

CREATE TABLE fill (
  id               BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
  client_order_id  TEXT NOT NULL             CHECK (client_order_id <> ''),
  px               NUMERIC NOT NULL          CHECK (px > 0),
  qty              NUMERIC NOT NULL          CHECK (qty > 0),
  fee              NUMERIC NOT NULL,
  event_ts         TIMESTAMPTZ NOT NULL,
  ingest_ts        TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp()
);
CREATE INDEX fill_client_order_id_event_ts_idx ON fill (client_order_id, event_ts);
COMMENT ON TABLE fill IS 'One fill reported by the broker, keyed to its order by client_order_id.';
SELECT nisse_make_append_only('fill');

CREATE TABLE position_event (
  id          BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
  symbol      TEXT NOT NULL                  CHECK (symbol <> ''),
  qty_after   NUMERIC NOT NULL,
  avg_cost    NUMERIC NOT NULL               CHECK (avg_cost >= 0),
  event_ts    TIMESTAMPTZ NOT NULL,
  ingest_ts   TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp()
);
CREATE INDEX position_event_symbol_event_ts_idx ON position_event (symbol, event_ts);
COMMENT ON TABLE position_event IS 'The position in a symbol after a change; qty_after may be negative (short).';
SELECT nisse_make_append_only('position_event');
