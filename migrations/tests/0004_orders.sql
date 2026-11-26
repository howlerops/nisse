-- Schema checks for order_event, fill and position_event. Runs inside the runner's transaction.

-- Parent rows for the decision foreign key (same valid inserts as 0003).
INSERT INTO strategy_version (git_sha, config_hash, params, event_ts)
VALUES (repeat('a', 40), decode(repeat('ab', 32), 'hex'), '{}', '2026-01-02T00:00:00Z');

INSERT INTO decision (strategy_version_id, symbol, action, intended_qty, expected_edge_bps,
                      expected_vol, features, feature_hash, data_snapshot_id, rng_seed,
                      reason_code, event_ts)
VALUES ((SELECT id FROM strategy_version WHERE git_sha = repeat('a', 40)),
        'SPY', 'enter', 1.5, 12.5, 0.15, '{"mom_12m": 0.1}', decode(repeat('ab', 32), 'hex'),
        'snap-1', 42, 'trend_up', '2026-01-05T21:00:00Z');

-- 1. Intent row (broker_order_id NULL) first, then the broker's accepted row later.
INSERT INTO order_event (decision_id, client_order_id, broker_order_id, state, type, limit_px, qty,
                         symbol, side, time_in_force, cum_filled_qty, event_ts)
VALUES ((SELECT min(id) FROM decision), 'nisse-0001', NULL, 'intent', 'LIMIT', 500.10, 1.5,
        'SPY', 'BUY', 'DAY', 0, '2026-01-05T21:00:00Z');

INSERT INTO order_event (decision_id, client_order_id, broker_order_id, state, type, limit_px, qty,
                         symbol, side, time_in_force, cum_filled_qty, event_ts)
VALUES ((SELECT min(id) FROM decision), 'nisse-0001', 'brk-1', 'accepted', 'LIMIT', 500.10, 1.5,
        'SPY', 'BUY', 'DAY', 0, '2026-01-05T21:00:01Z');

SELECT pg_temp.assert_true(
  (SELECT broker_order_id IS NULL AND state = 'intent' FROM order_event
    WHERE client_order_id = 'nisse-0001' ORDER BY event_ts, id LIMIT 1),
  'earliest order_event for a client_order_id must be the intent row with broker_order_id NULL');

-- 2. A market order has no limit price.
INSERT INTO order_event (decision_id, client_order_id, state, type, limit_px, qty,
                         symbol, side, time_in_force, cum_filled_qty, event_ts)
VALUES ((SELECT min(id) FROM decision), 'nisse-0003', 'intent', 'MARKET', NULL, 2,
        'SPY', 'BUY', 'DAY', 0, '2026-01-05T21:00:02Z');

SELECT pg_temp.assert_true(
  (SELECT limit_px IS NULL FROM order_event WHERE client_order_id = 'nisse-0003'),
  'market order_event must accept a NULL limit_px');

-- 3. order_event rejections.
SELECT pg_temp.assert_raises($q$
  INSERT INTO order_event (decision_id, client_order_id, state, type, limit_px, qty,
                           symbol, side, time_in_force, cum_filled_qty, event_ts)
  VALUES ((SELECT min(id) FROM decision), '', 'intent', 'LIMIT', 500.10, 1,
          'SPY', 'BUY', 'DAY', 0, '2026-01-05T21:00:00Z')
$q$, '23514');

SELECT pg_temp.assert_raises($q$
  INSERT INTO order_event (decision_id, client_order_id, state, type, limit_px, qty,
                           symbol, side, time_in_force, cum_filled_qty, event_ts)
  VALUES ((SELECT min(id) FROM decision), 'nisse-0004', '', 'LIMIT', 500.10, 1,
          'SPY', 'BUY', 'DAY', 0, '2026-01-05T21:00:00Z')
$q$, '23514');

SELECT pg_temp.assert_raises($q$
  INSERT INTO order_event (decision_id, client_order_id, state, type, limit_px, qty,
                           symbol, side, time_in_force, cum_filled_qty, event_ts)
  VALUES ((SELECT min(id) FROM decision), 'nisse-0004', 'intent', '', NULL, 1,
          'SPY', 'BUY', 'DAY', 0, '2026-01-05T21:00:00Z')
$q$, '23514');

SELECT pg_temp.assert_raises($q$
  INSERT INTO order_event (decision_id, client_order_id, state, type, limit_px, qty,
                           symbol, side, time_in_force, cum_filled_qty, event_ts)
  VALUES ((SELECT min(id) FROM decision), 'nisse-0004', 'intent', 'LIMIT', 500.10, 0,
          'SPY', 'BUY', 'DAY', 0, '2026-01-05T21:00:00Z')
$q$, '23514');

SELECT pg_temp.assert_raises($q$
  INSERT INTO order_event (decision_id, client_order_id, state, type, limit_px, qty,
                           symbol, side, time_in_force, cum_filled_qty, event_ts)
  VALUES ((SELECT min(id) FROM decision), 'nisse-0004', 'intent', 'LIMIT', 500.10, -1,
          'SPY', 'BUY', 'DAY', 0, '2026-01-05T21:00:00Z')
$q$, '23514');

SELECT pg_temp.assert_raises($q$
  INSERT INTO order_event (decision_id, client_order_id, state, type, limit_px, qty,
                           symbol, side, time_in_force, cum_filled_qty, event_ts)
  VALUES ((SELECT min(id) FROM decision), 'nisse-0004', 'intent', 'LIMIT', 0, 1,
          'SPY', 'BUY', 'DAY', 0, '2026-01-05T21:00:00Z')
$q$, '23514');

SELECT pg_temp.assert_raises($q$
  INSERT INTO order_event (decision_id, client_order_id, state, type, limit_px, qty,
                           symbol, side, time_in_force, cum_filled_qty, event_ts)
  VALUES (999999, 'nisse-0004', 'intent', 'LIMIT', 500.10, 1,
          'SPY', 'BUY', 'DAY', 0, '2026-01-05T21:00:00Z')
$q$, '23503');

SELECT pg_temp.assert_raises($q$
  INSERT INTO order_event (decision_id, client_order_id, state, type, limit_px, qty,
                           symbol, side, time_in_force, cum_filled_qty, event_ts)
  VALUES ((SELECT min(id) FROM decision), NULL, 'intent', 'LIMIT', 500.10, 1,
          'SPY', 'BUY', 'DAY', 0, '2026-01-05T21:00:00Z')
$q$, '23502');

SELECT pg_temp.assert_raises($q$
  INSERT INTO order_event (decision_id, client_order_id, state, type, limit_px, qty,
                           symbol, side, time_in_force, cum_filled_qty, event_ts)
  VALUES ((SELECT min(id) FROM decision), 'nisse-0004', 'intent', 'LIMIT', 500.10, NULL,
          'SPY', 'BUY', 'DAY', 0, '2026-01-05T21:00:00Z')
$q$, '23502');

SELECT pg_temp.assert_raises($q$
  INSERT INTO order_event (decision_id, client_order_id, state, type, limit_px, qty,
                           symbol, side, time_in_force, cum_filled_qty, event_ts)
  VALUES ((SELECT min(id) FROM decision), 'nisse-0004', 'intent', 'LIMIT', 500.10, 1,
          'SPY', 'BUY', 'DAY', 0, NULL)
$q$, '23502');

-- id is GENERATED ALWAYS, so an explicit id is rejected.
SELECT pg_temp.assert_raises($q$
  INSERT INTO order_event (id, decision_id, client_order_id, state, type, limit_px, qty,
                           symbol, side, time_in_force, cum_filled_qty, event_ts)
  VALUES (424242, (SELECT min(id) FROM decision), 'nisse-0004', 'intent', 'LIMIT', 500.10, 1,
          'SPY', 'BUY', 'DAY', 0, '2026-01-05T21:00:00Z')
$q$, '428C9');

-- 4. Two partial fills for one order; cumulative quantity is the sum.
INSERT INTO fill (broker_fill_id, client_order_id, px, qty, fee, event_ts)
VALUES ('exec-1-1', 'nisse-0001', 500.10, 0.5, 0, '2026-01-05T21:00:02Z'),
       ('exec-1-2', 'nisse-0001', 500.09, 1.0, -0.01, '2026-01-05T21:00:03Z');

SELECT pg_temp.assert_true(
  (SELECT sum(qty) FROM fill WHERE client_order_id = 'nisse-0001') = 1.5,
  'cumulative filled quantity must equal the sum of the fills exactly');

SELECT pg_temp.assert_true(
  (SELECT count(*) = 1 FROM fill WHERE client_order_id = 'nisse-0001' AND fee = -0.01),
  'a negative fee (rebate) must be stored');

-- 5. NUMERIC is exact: 0.1 + 0.2 is 0.3 (false with floating point).
INSERT INTO fill (broker_fill_id, client_order_id, px, qty, fee, event_ts)
VALUES ('exec-2-1', 'nisse-0002', 0.1, 0.2, 0, '2026-01-05T21:00:04Z');

SELECT pg_temp.assert_true(
  (SELECT px + qty = 0.3 FROM fill WHERE client_order_id = 'nisse-0002'),
  'fill px and qty must be exact NUMERIC values');

-- 6. fill rejections.
SELECT pg_temp.assert_raises($q$
  INSERT INTO fill (broker_fill_id, client_order_id, px, qty, fee, event_ts)
  VALUES ('exec-3-1', 'nisse-0001', 0, 1, 0, '2026-01-05T21:00:00Z')
$q$, '23514');

SELECT pg_temp.assert_raises($q$
  INSERT INTO fill (broker_fill_id, client_order_id, px, qty, fee, event_ts)
  VALUES ('exec-4-1', 'nisse-0001', 500.10, 0, 0, '2026-01-05T21:00:00Z')
$q$, '23514');

SELECT pg_temp.assert_raises($q$
  INSERT INTO fill (broker_fill_id, client_order_id, px, qty, fee, event_ts)
  VALUES ('exec-5-1', 'nisse-0001', 500.10, -1, 0, '2026-01-05T21:00:00Z')
$q$, '23514');

SELECT pg_temp.assert_raises($q$
  INSERT INTO fill (broker_fill_id, client_order_id, px, qty, fee, event_ts)
  VALUES ('exec-6-1', '', 500.10, 1, 0, '2026-01-05T21:00:00Z')
$q$, '23514');

SELECT pg_temp.assert_raises($q$
  INSERT INTO fill (broker_fill_id, client_order_id, px, qty, fee, event_ts)
  VALUES ('exec-7-1', 'nisse-0001', 500.10, 1, NULL, '2026-01-05T21:00:00Z')
$q$, '23502');

SELECT pg_temp.assert_raises($q$
  INSERT INTO fill (broker_fill_id, client_order_id, px, qty, fee, event_ts)
  VALUES ('exec-8-1', 'nisse-0001', NULL, 1, 0, '2026-01-05T21:00:00Z')
$q$, '23502');

SELECT pg_temp.assert_raises($q$
  INSERT INTO fill (broker_fill_id, client_order_id, px, qty, fee, event_ts)
  VALUES ('exec-9-1', 'nisse-0001', 500.10, 1, 0, NULL)
$q$, '23502');

-- 7. position_event: long, flat and short positions all succeed.
INSERT INTO position_event (symbol, qty_after, avg_cost, event_ts)
VALUES ('SPY', 1.5, 500.10, '2026-01-05T21:00:05Z'),
       ('SPY', 0, 0, '2026-01-05T21:00:06Z'),
       ('SPY', -2, 100, '2026-01-05T21:00:07Z');

SELECT pg_temp.assert_true(
  (SELECT count(*) = 3 FROM position_event WHERE symbol = 'SPY'),
  'position_event must accept long, flat and short positions');

-- 8. position_event rejections.
SELECT pg_temp.assert_raises($q$
  INSERT INTO position_event (symbol, qty_after, avg_cost, event_ts)
  VALUES ('SPY', 1, -1, '2026-01-05T21:00:00Z')
$q$, '23514');

SELECT pg_temp.assert_raises($q$
  INSERT INTO position_event (symbol, qty_after, avg_cost, event_ts)
  VALUES ('', 1, 1, '2026-01-05T21:00:00Z')
$q$, '23514');

SELECT pg_temp.assert_raises($q$
  INSERT INTO position_event (symbol, qty_after, avg_cost, event_ts)
  VALUES ('SPY', NULL, 1, '2026-01-05T21:00:00Z')
$q$, '23502');

SELECT pg_temp.assert_raises($q$
  INSERT INTO position_event (symbol, qty_after, avg_cost, event_ts)
  VALUES ('SPY', 1, NULL, '2026-01-05T21:00:00Z')
$q$, '23502');

SELECT pg_temp.assert_raises($q$
  INSERT INTO position_event (symbol, qty_after, avg_cost, event_ts)
  VALUES ('SPY', 1, 1, NULL)
$q$, '23502');

-- A fill delivered twice (same broker execution id) is rejected, so it cannot be counted twice;
-- NaN can never reach a price, a quantity or a risk limit.
SELECT pg_temp.assert_raises($q$
  INSERT INTO fill (broker_fill_id, client_order_id, px, qty, fee, event_ts)
  VALUES ('exec-1-1', 'nisse-0001', 500.10, 0.5, 0, '2026-01-05T21:00:02Z')
$q$, '23505');

SELECT pg_temp.assert_raises($q$
  INSERT INTO fill (broker_fill_id, client_order_id, px, qty, fee, event_ts)
  VALUES ('', 'nisse-0001', 500.10, 0.5, 0, '2026-01-05T21:00:02Z')
$q$, '23514');

SELECT pg_temp.assert_raises($q$
  INSERT INTO fill (broker_fill_id, client_order_id, px, qty, fee, event_ts)
  VALUES ('exec-nan', 'nisse-0001', 'NaN', 'Infinity', 'NaN', '2026-01-05T21:00:02Z')
$q$, '23514');

SELECT pg_temp.assert_raises($q$
  INSERT INTO order_event (decision_id, client_order_id, state, type, limit_px, qty,
                           symbol, side, time_in_force, cum_filled_qty, event_ts)
  VALUES ((SELECT min(id) FROM decision), 'nisse-0009', 'intent', 'MARKET', NULL, 'NaN',
          'SPY', 'BUY', 'DAY', 0, '2026-01-05T21:00:00Z')
$q$, '23514');

SELECT pg_temp.assert_raises($q$
  INSERT INTO risk_check (decision_id, rule, limit_value, actual_value, passed, event_ts)
  VALUES ((SELECT min(id) FROM decision), 'max_position_pct', 'NaN', 'Infinity', true,
          '2026-01-05T21:00:00Z')
$q$, '23514');

-- 9. All three tables are append-only (checked after rows exist).
SELECT pg_temp.assert_append_only('order_event');
SELECT pg_temp.assert_append_only('fill');
SELECT pg_temp.assert_append_only('position_event');
