-- Schema checks for the recovery columns and value lists added to order_event by 0006.
-- Runs inside the runner's transaction.

-- Parent rows for the decision foreign key (same valid inserts as 0004).
INSERT INTO strategy_version (git_sha, config_hash, params, event_ts)
VALUES (repeat('a', 40), decode(repeat('ab', 32), 'hex'), '{}', '2026-01-02T00:00:00Z');

INSERT INTO decision (strategy_version_id, symbol, action, intended_qty, expected_edge_bps,
                      expected_vol, features, feature_hash, data_snapshot_id, rng_seed,
                      reason_code, event_ts)
VALUES ((SELECT id FROM strategy_version WHERE git_sha = repeat('a', 40)),
        'SPY', 'enter', 1.5, 12.5, 0.15, '{"mom_12m": 0.1}', decode(repeat('ab', 32), 'hex'),
        'snap-1', 42, 'trend_up', '2026-01-05T21:00:00Z');

-- 1. A valid LIMIT intent and a valid MARKET order succeed; an intent row has cum_filled_qty 0.
INSERT INTO order_event (decision_id, client_order_id, state, type, limit_px, qty, cum_filled_qty,
                         symbol, side, time_in_force, event_ts)
VALUES ((SELECT min(id) FROM decision), 'nisse-7001', 'intent', 'LIMIT', 500.10, 1.5, 0,
        'SPY', 'BUY', 'DAY', '2026-01-05T21:00:00Z');

INSERT INTO order_event (decision_id, client_order_id, state, type, limit_px, qty, cum_filled_qty,
                         symbol, side, time_in_force, event_ts)
VALUES ((SELECT min(id) FROM decision), 'nisse-7002', 'intent', 'MARKET', NULL, 1.5, 0,
        'SPY', 'SELL', 'GTC', '2026-01-05T21:00:01Z');

SELECT pg_temp.assert_true(
  (SELECT cum_filled_qty = 0 FROM order_event
    WHERE client_order_id = 'nisse-7001' AND state = 'intent'),
  'an intent order_event must have cum_filled_qty = 0');

-- 2. Every allowed state is accepted.
INSERT INTO order_event (decision_id, client_order_id, state, type, limit_px, qty, cum_filled_qty,
                         symbol, side, time_in_force, event_ts)
VALUES ((SELECT min(id) FROM decision), 'nisse-7101', 'intent',         'LIMIT', 500.10, 1.5, 0, 'SPY', 'BUY', 'DAY', '2026-01-05T21:00:10Z'),
       ((SELECT min(id) FROM decision), 'nisse-7102', 'submitted',      'LIMIT', 500.10, 1.5, 0, 'SPY', 'BUY', 'DAY', '2026-01-05T21:00:11Z'),
       ((SELECT min(id) FROM decision), 'nisse-7103', 'accepted',       'LIMIT', 500.10, 1.5, 0, 'SPY', 'BUY', 'DAY', '2026-01-05T21:00:12Z'),
       ((SELECT min(id) FROM decision), 'nisse-7104', 'partially_filled', 'LIMIT', 500.10, 1.5, 0, 'SPY', 'BUY', 'DAY', '2026-01-05T21:00:13Z'),
       ((SELECT min(id) FROM decision), 'nisse-7105', 'pending_cancel', 'LIMIT', 500.10, 1.5, 0, 'SPY', 'BUY', 'DAY', '2026-01-05T21:00:14Z'),
       ((SELECT min(id) FROM decision), 'nisse-7106', 'filled',         'LIMIT', 500.10, 1.5, 0, 'SPY', 'BUY', 'DAY', '2026-01-05T21:00:15Z'),
       ((SELECT min(id) FROM decision), 'nisse-7107', 'canceled',       'LIMIT', 500.10, 1.5, 0, 'SPY', 'BUY', 'DAY', '2026-01-05T21:00:16Z'),
       ((SELECT min(id) FROM decision), 'nisse-7108', 'replaced',       'LIMIT', 500.10, 1.5, 0, 'SPY', 'BUY', 'DAY', '2026-01-05T21:00:17Z'),
       ((SELECT min(id) FROM decision), 'nisse-7109', 'rejected',       'LIMIT', 500.10, 1.5, 0, 'SPY', 'BUY', 'DAY', '2026-01-05T21:00:18Z'),
       ((SELECT min(id) FROM decision), 'nisse-7110', 'expired',        'LIMIT', 500.10, 1.5, 0, 'SPY', 'BUY', 'DAY', '2026-01-05T21:00:19Z');

SELECT pg_temp.assert_true(
  (SELECT count(DISTINCT state) = 10 FROM order_event WHERE client_order_id BETWEEN 'nisse-7101' AND 'nisse-7110'),
  'all ten allowed states must be accepted');

-- 3. Partial and complete fills: cum_filled_qty may equal qty, and may sit below it.
INSERT INTO order_event (decision_id, client_order_id, state, type, limit_px, qty, cum_filled_qty,
                         symbol, side, time_in_force, event_ts)
VALUES ((SELECT min(id) FROM decision), 'nisse-7201', 'partially_filled', 'LIMIT', 500.10, 1.5, 0.5,
        'SPY', 'BUY', 'DAY', '2026-01-05T21:00:20Z'),
       ((SELECT min(id) FROM decision), 'nisse-7202', 'filled', 'LIMIT', 500.10, 1.5, 1.5,
        'SPY', 'BUY', 'DAY', '2026-01-05T21:00:21Z');

-- 4. Rejections with SQLSTATE 23514. Every other column is valid, so each row fails for one reason.
SELECT pg_temp.assert_raises($q$
  INSERT INTO order_event (decision_id, client_order_id, state, type, limit_px, qty, cum_filled_qty,
                           symbol, side, time_in_force, event_ts)
  VALUES ((SELECT min(id) FROM decision), 'nisse-7301', 'new', 'LIMIT', 500.10, 1.5, 0,
          'SPY', 'BUY', 'DAY', '2026-01-05T21:00:00Z')
$q$, '23514');

SELECT pg_temp.assert_raises($q$
  INSERT INTO order_event (decision_id, client_order_id, state, type, limit_px, qty, cum_filled_qty,
                           symbol, side, time_in_force, event_ts)
  VALUES ((SELECT min(id) FROM decision), 'nisse-7302', 'Intent', 'LIMIT', 500.10, 1.5, 0,
          'SPY', 'BUY', 'DAY', '2026-01-05T21:00:00Z')
$q$, '23514');

SELECT pg_temp.assert_raises($q$
  INSERT INTO order_event (decision_id, client_order_id, state, type, limit_px, qty, cum_filled_qty,
                           symbol, side, time_in_force, event_ts)
  VALUES ((SELECT min(id) FROM decision), 'nisse-7303', 'intent', 'limit', NULL, 1.5, 0,
          'SPY', 'BUY', 'DAY', '2026-01-05T21:00:00Z')
$q$, '23514');

SELECT pg_temp.assert_raises($q$
  INSERT INTO order_event (decision_id, client_order_id, state, type, limit_px, qty, cum_filled_qty,
                           symbol, side, time_in_force, event_ts)
  VALUES ((SELECT min(id) FROM decision), 'nisse-7304', 'intent', 'STOP', NULL, 1.5, 0,
          'SPY', 'BUY', 'DAY', '2026-01-05T21:00:00Z')
$q$, '23514');

SELECT pg_temp.assert_raises($q$
  INSERT INTO order_event (decision_id, client_order_id, state, type, limit_px, qty, cum_filled_qty,
                           symbol, side, time_in_force, event_ts)
  VALUES ((SELECT min(id) FROM decision), 'nisse-7305', 'intent', 'LIMIT', 500.10, 1.5, 0,
          'SPY', 'buy', 'DAY', '2026-01-05T21:00:00Z')
$q$, '23514');

SELECT pg_temp.assert_raises($q$
  INSERT INTO order_event (decision_id, client_order_id, state, type, limit_px, qty, cum_filled_qty,
                           symbol, side, time_in_force, event_ts)
  VALUES ((SELECT min(id) FROM decision), 'nisse-7306', 'intent', 'LIMIT', 500.10, 1.5, 0,
          'SPY', 'HOLD', 'DAY', '2026-01-05T21:00:00Z')
$q$, '23514');

SELECT pg_temp.assert_raises($q$
  INSERT INTO order_event (decision_id, client_order_id, state, type, limit_px, qty, cum_filled_qty,
                           symbol, side, time_in_force, event_ts)
  VALUES ((SELECT min(id) FROM decision), 'nisse-7307', 'intent', 'LIMIT', 500.10, 1.5, 0,
          'SPY', 'BUY', 'IOC', '2026-01-05T21:00:00Z')
$q$, '23514');

SELECT pg_temp.assert_raises($q$
  INSERT INTO order_event (decision_id, client_order_id, state, type, limit_px, qty, cum_filled_qty,
                           symbol, side, time_in_force, event_ts)
  VALUES ((SELECT min(id) FROM decision), 'nisse-7308', 'intent', 'LIMIT', 500.10, 1.5, 0,
          '', 'BUY', 'DAY', '2026-01-05T21:00:00Z')
$q$, '23514');

-- qty -1 also breaks cum_filled_qty <= qty; no cumulative value can satisfy both checks here.
SELECT pg_temp.assert_raises($q$
  INSERT INTO order_event (decision_id, client_order_id, state, type, limit_px, qty, cum_filled_qty,
                           symbol, side, time_in_force, event_ts)
  VALUES ((SELECT min(id) FROM decision), 'nisse-7309', 'intent', 'LIMIT', 500.10, 1.5, -1,
          'SPY', 'BUY', 'DAY', '2026-01-05T21:00:00Z')
$q$, '23514');

SELECT pg_temp.assert_raises($q$
  INSERT INTO order_event (decision_id, client_order_id, state, type, limit_px, qty, cum_filled_qty,
                           symbol, side, time_in_force, event_ts)
  VALUES ((SELECT min(id) FROM decision), 'nisse-7310', 'partially_filled', 'LIMIT', 500.10, 1.5, 2,
          'SPY', 'BUY', 'DAY', '2026-01-05T21:00:00Z')
$q$, '23514');

SELECT pg_temp.assert_raises($q$
  INSERT INTO order_event (decision_id, client_order_id, state, type, limit_px, qty, cum_filled_qty,
                           symbol, side, time_in_force, event_ts)
  VALUES ((SELECT min(id) FROM decision), 'nisse-7311', 'partially_filled', 'LIMIT', 500.10, 1.5,
          'NaN', 'SPY', 'BUY', 'DAY', '2026-01-05T21:00:00Z')
$q$, '23514');

SELECT pg_temp.assert_raises($q$
  INSERT INTO order_event (decision_id, client_order_id, state, type, limit_px, qty, cum_filled_qty,
                           symbol, side, time_in_force, event_ts)
  VALUES ((SELECT min(id) FROM decision), 'nisse-7312', 'intent', 'LIMIT', NULL, 1.5, 0,
          'SPY', 'BUY', 'DAY', '2026-01-05T21:00:00Z')
$q$, '23514');

SELECT pg_temp.assert_raises($q$
  INSERT INTO order_event (decision_id, client_order_id, state, type, limit_px, qty, cum_filled_qty,
                           symbol, side, time_in_force, event_ts)
  VALUES ((SELECT min(id) FROM decision), 'nisse-7313', 'intent', 'MARKET', 500.10, 1.5, 0,
          'SPY', 'BUY', 'DAY', '2026-01-05T21:00:00Z')
$q$, '23514');

-- 5. Rejections with SQLSTATE 23502 (NOT NULL).
SELECT pg_temp.assert_raises($q$
  INSERT INTO order_event (decision_id, client_order_id, state, type, limit_px, qty, cum_filled_qty,
                           symbol, side, time_in_force, event_ts)
  VALUES ((SELECT min(id) FROM decision), 'nisse-7401', 'intent', 'LIMIT', 500.10, 1.5, 0,
          NULL, 'BUY', 'DAY', '2026-01-05T21:00:00Z')
$q$, '23502');

SELECT pg_temp.assert_raises($q$
  INSERT INTO order_event (decision_id, client_order_id, state, type, limit_px, qty, cum_filled_qty,
                           symbol, side, time_in_force, event_ts)
  VALUES ((SELECT min(id) FROM decision), 'nisse-7402', 'intent', 'LIMIT', 500.10, 1.5, 0,
          'SPY', NULL, 'DAY', '2026-01-05T21:00:00Z')
$q$, '23502');

SELECT pg_temp.assert_raises($q$
  INSERT INTO order_event (decision_id, client_order_id, state, type, limit_px, qty, cum_filled_qty,
                           symbol, side, time_in_force, event_ts)
  VALUES ((SELECT min(id) FROM decision), 'nisse-7403', 'intent', 'LIMIT', 500.10, 1.5, 0,
          'SPY', 'BUY', NULL, '2026-01-05T21:00:00Z')
$q$, '23502');

SELECT pg_temp.assert_raises($q$
  INSERT INTO order_event (decision_id, client_order_id, state, type, limit_px, qty, cum_filled_qty,
                           symbol, side, time_in_force, event_ts)
  VALUES ((SELECT min(id) FROM decision), 'nisse-7404', 'intent', 'LIMIT', 500.10, 1.5, NULL,
          'SPY', 'BUY', 'DAY', '2026-01-05T21:00:00Z')
$q$, '23502');

-- 6. order_event stays append-only with the new columns in place.
SELECT pg_temp.assert_append_only('order_event');
