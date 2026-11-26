-- These columns make an order_event row self-contained: the write-ahead intent row alone is enough
--   to recover an order after a crash (symbol, side and time in force no longer need a join).
-- cum_filled_qty is the broker's cumulative filled quantity at that event (0 on the intent row).
-- state values mirror nisse_core::OrderState; side, type and time_in_force hold the NautilusTrader
--   enum strings.
-- Columns can only be appended, so the new columns land after ingest_ts in column order.

ALTER TABLE order_event
  ADD COLUMN symbol          TEXT NOT NULL            CHECK (symbol <> ''),
  ADD COLUMN side            TEXT NOT NULL            CHECK (side IN ('BUY', 'SELL')),
  ADD COLUMN time_in_force   TEXT NOT NULL            CHECK (time_in_force IN ('DAY', 'GTC')),
  ADD COLUMN cum_filled_qty  finite_numeric NOT NULL  CHECK (cum_filled_qty >= 0),
  ADD CONSTRAINT order_event_state_check2 CHECK (state IN ('intent', 'submitted', 'accepted', 'partially_filled', 'pending_cancel', 'filled', 'canceled', 'replaced', 'rejected', 'expired')),
  ADD CONSTRAINT order_event_type_check2 CHECK (type IN ('MARKET', 'LIMIT')),
  ADD CONSTRAINT order_event_cum_le_qty CHECK (cum_filled_qty <= qty),
  ADD CONSTRAINT order_event_limit_px_iff_limit CHECK ((type = 'LIMIT') = (limit_px IS NOT NULL));
