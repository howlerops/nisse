-- One intent row per client order id.
--
-- The intent row is the write-ahead record of an order and the row a restart rebuilds the
-- order from. Two of them for one client order id would mean the id was reused, and the
-- broker would treat the second submit as a duplicate of the first. The engine's in-memory
-- tracker already refuses a reused id, but it only knows the orders it has loaded; this makes
-- the database refuse it too. The intent insert is the one awaited write before an order is
-- sent, and a partial unique index adds one small index update to it.

CREATE UNIQUE INDEX order_event_one_intent_per_order
  ON order_event (client_order_id)
  WHERE state = 'intent';
