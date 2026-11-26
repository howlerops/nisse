-- A client order id can have many rows but only one intent row.

INSERT INTO strategy_version (git_sha, config_hash, params, event_ts)
VALUES (repeat('a', 40), decode(repeat('ab', 32), 'hex'), '{}', '2026-01-02T00:00:00Z');

INSERT INTO decision (strategy_version_id, symbol, action, intended_qty, features, feature_hash,
                      data_snapshot_id, rng_seed, reason_code, event_ts)
VALUES ((SELECT id FROM strategy_version WHERE git_sha = repeat('a', 40)),
        'SPY', 'enter', 1.5, '{}', decode(repeat('ab', 32), 'hex'), 'snap-1', 42, 'trend_up',
        '2026-01-05T21:00:00Z');

INSERT INTO order_event (decision_id, client_order_id, state, type, limit_px, qty, symbol, side,
                         time_in_force, cum_filled_qty, event_ts)
VALUES ((SELECT min(id) FROM decision), 'nisse-1-0', 'intent', 'LIMIT', 500.10, 1.5, 'SPY', 'BUY',
        'DAY', 0, '2026-01-05T21:00:00Z');

-- Later states for the same order are fine, including repeats of the same state.
INSERT INTO order_event (decision_id, client_order_id, broker_order_id, state, type, limit_px, qty,
                         symbol, side, time_in_force, cum_filled_qty, event_ts)
VALUES ((SELECT min(id) FROM decision), 'nisse-1-0', 'brk-1', 'accepted', 'LIMIT', 500.10, 1.5,
        'SPY', 'BUY', 'DAY', 0, '2026-01-05T21:00:01Z'),
       ((SELECT min(id) FROM decision), 'nisse-1-0', 'brk-1', 'accepted', 'LIMIT', 500.10, 1.5,
        'SPY', 'BUY', 'DAY', 0, '2026-01-05T21:00:02Z');

-- A second intent for the same client order id is refused.
SELECT pg_temp.assert_raises($q$
  INSERT INTO order_event (decision_id, client_order_id, state, type, limit_px, qty, symbol, side,
                           time_in_force, cum_filled_qty, event_ts)
  VALUES ((SELECT min(id) FROM decision), 'nisse-1-0', 'intent', 'LIMIT', 500.10, 1.5, 'SPY',
          'BUY', 'DAY', 0, '2026-01-05T21:00:03Z')
$q$, '23505');

-- A different client order id can have its own intent.
INSERT INTO order_event (decision_id, client_order_id, state, type, limit_px, qty, symbol, side,
                         time_in_force, cum_filled_qty, event_ts)
VALUES ((SELECT min(id) FROM decision), 'nisse-1-1', 'intent', 'LIMIT', 500.10, 1.5, 'SPY', 'BUY',
        'DAY', 0, '2026-01-05T21:00:04Z');

SELECT pg_temp.assert_true(
  (SELECT count(*) = 2 FROM order_event WHERE state = 'intent'),
  'exactly one intent row per client order id');
