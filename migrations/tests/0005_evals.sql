-- Schema checks for outcome_window and eval_score. Runs inside the runner's transaction.

-- Parent rows for the foreign keys (same valid insert as 0002 and 0003, own snapshot id).
INSERT INTO strategy_version (git_sha, config_hash, params, event_ts)
VALUES (repeat('a', 40), decode(repeat('ab', 32), 'hex'), '{}', '2026-01-02T00:00:00Z');

INSERT INTO decision (strategy_version_id, symbol, action, intended_qty, features, feature_hash,
                      data_snapshot_id, rng_seed, reason_code, event_ts)
VALUES ((SELECT id FROM strategy_version WHERE git_sha = repeat('a', 40)),
        'SPY', 'enter', 1, '{}', decode(repeat('ab', 32), 'hex'),
        'snap-evals', 42, 'trend_up', '2026-01-05T21:00:00Z');

-- 1. outcome_window: one row per fixed horizon for the decision, one with a negative forward return.
INSERT INTO outcome_window (decision_id, horizon, fwd_ret, bench_ret, mfe, mae, event_ts)
VALUES ((SELECT id FROM decision WHERE data_snapshot_id = 'snap-evals'),
        INTERVAL '1 day', 0.012, 0.004, 0.02, -0.01, '2026-02-05T21:00:00Z'),
       ((SELECT id FROM decision WHERE data_snapshot_id = 'snap-evals'),
        INTERVAL '5 days', -0.003, 0.004, 0.02, -0.01, '2026-02-05T21:00:00Z'),
       ((SELECT id FROM decision WHERE data_snapshot_id = 'snap-evals'),
        INTERVAL '21 days', 0.012, 0.004, 0.02, -0.01, '2026-02-05T21:00:00Z');

SELECT pg_temp.assert_true(
  (SELECT count(*) = 3 FROM outcome_window
    WHERE decision_id = (SELECT id FROM decision WHERE data_snapshot_id = 'snap-evals')),
  'outcome_window must accept one row per fixed horizon for a decision');

SELECT pg_temp.assert_true(
  (SELECT ingest_ts IS NOT NULL FROM outcome_window WHERE horizon = INTERVAL '1 day'),
  'outcome_window.ingest_ts must default to clock_timestamp()');

-- 2. outcome_window rejections.
SELECT pg_temp.assert_raises($q$
  INSERT INTO outcome_window (decision_id, horizon, fwd_ret, bench_ret, mfe, mae, event_ts)
  VALUES ((SELECT id FROM decision WHERE data_snapshot_id = 'snap-evals'),
          INTERVAL '0', 0.012, 0.004, 0.02, -0.01, '2026-02-05T21:00:00Z')
$q$, '23514');

SELECT pg_temp.assert_raises($q$
  INSERT INTO outcome_window (decision_id, horizon, fwd_ret, bench_ret, mfe, mae, event_ts)
  VALUES ((SELECT id FROM decision WHERE data_snapshot_id = 'snap-evals'),
          INTERVAL '-1 day', 0.012, 0.004, 0.02, -0.01, '2026-02-05T21:00:00Z')
$q$, '23514');

SELECT pg_temp.assert_raises($q$
  INSERT INTO outcome_window (decision_id, horizon, fwd_ret, bench_ret, mfe, mae, event_ts)
  VALUES (999999, INTERVAL '1 day', 0.012, 0.004, 0.02, -0.01, '2026-02-05T21:00:00Z')
$q$, '23503');

SELECT pg_temp.assert_raises($q$
  INSERT INTO outcome_window (decision_id, horizon, fwd_ret, bench_ret, mfe, mae, event_ts)
  VALUES ((SELECT id FROM decision WHERE data_snapshot_id = 'snap-evals'),
          NULL, 0.012, 0.004, 0.02, -0.01, '2026-02-05T21:00:00Z')
$q$, '23502');

SELECT pg_temp.assert_raises($q$
  INSERT INTO outcome_window (decision_id, horizon, fwd_ret, bench_ret, mfe, mae, event_ts)
  VALUES ((SELECT id FROM decision WHERE data_snapshot_id = 'snap-evals'),
          INTERVAL '1 day', NULL, 0.004, 0.02, -0.01, '2026-02-05T21:00:00Z')
$q$, '23502');

SELECT pg_temp.assert_raises($q$
  INSERT INTO outcome_window (decision_id, horizon, fwd_ret, bench_ret, mfe, mae, event_ts)
  VALUES ((SELECT id FROM decision WHERE data_snapshot_id = 'snap-evals'),
          INTERVAL '1 day', 0.012, NULL, 0.02, -0.01, '2026-02-05T21:00:00Z')
$q$, '23502');

SELECT pg_temp.assert_raises($q$
  INSERT INTO outcome_window (decision_id, horizon, fwd_ret, bench_ret, mfe, mae, event_ts)
  VALUES ((SELECT id FROM decision WHERE data_snapshot_id = 'snap-evals'),
          INTERVAL '1 day', 0.012, 0.004, NULL, -0.01, '2026-02-05T21:00:00Z')
$q$, '23502');

SELECT pg_temp.assert_raises($q$
  INSERT INTO outcome_window (decision_id, horizon, fwd_ret, bench_ret, mfe, mae, event_ts)
  VALUES ((SELECT id FROM decision WHERE data_snapshot_id = 'snap-evals'),
          INTERVAL '1 day', 0.012, 0.004, 0.02, NULL, '2026-02-05T21:00:00Z')
$q$, '23502');

SELECT pg_temp.assert_raises($q$
  INSERT INTO outcome_window (decision_id, horizon, fwd_ret, bench_ret, mfe, mae, event_ts)
  VALUES ((SELECT id FROM decision WHERE data_snapshot_id = 'snap-evals'),
          INTERVAL '1 day', 0.012, 0.004, 0.02, -0.01, NULL)
$q$, '23502');

-- id is GENERATED ALWAYS, so an explicit id is rejected.
SELECT pg_temp.assert_raises($q$
  INSERT INTO outcome_window (id, decision_id, horizon, fwd_ret, bench_ret, mfe, mae, event_ts)
  VALUES (424242, (SELECT id FROM decision WHERE data_snapshot_id = 'snap-evals'),
          INTERVAL '1 day', 0.012, 0.004, 0.02, -0.01, '2026-02-05T21:00:00Z')
$q$, '428C9');

-- 3. eval_score: one row for a decision and one for a strategy version.
INSERT INTO eval_score (subject_id, subject_type, metric, value, eval_version, event_ts)
VALUES ((SELECT id FROM decision WHERE data_snapshot_id = 'snap-evals'),
        'decision', 'decision_quality', 0.35, 'v1', '2026-02-05T21:00:00Z'),
       ((SELECT id FROM strategy_version WHERE git_sha = repeat('a', 40)),
        'strategy_version', 'deflated_sharpe', 0.96, 'v1', '2026-02-05T21:00:00Z');

-- 4. A recomputation is a new row; the latest one by event_ts wins.
INSERT INTO eval_score (subject_id, subject_type, metric, value, eval_version, event_ts)
VALUES ((SELECT id FROM strategy_version WHERE git_sha = repeat('a', 40)),
        'strategy_version', 'deflated_sharpe', 0.91, 'v1', '2026-02-06T21:00:00Z');

SELECT pg_temp.assert_true(
  (SELECT count(*) = 2 FROM eval_score
    WHERE subject_type = 'strategy_version'
      AND subject_id = (SELECT id FROM strategy_version WHERE git_sha = repeat('a', 40))
      AND metric = 'deflated_sharpe' AND eval_version = 'v1'),
  'a recomputation must be stored as a new eval_score row');

SELECT pg_temp.assert_true(
  (SELECT value = 0.91 FROM eval_score
    WHERE subject_type = 'strategy_version'
      AND subject_id = (SELECT id FROM strategy_version WHERE git_sha = repeat('a', 40))
      AND metric = 'deflated_sharpe' AND eval_version = 'v1'
    ORDER BY event_ts DESC, id DESC LIMIT 1),
  'the latest deflated_sharpe row by event_ts must hold the recomputed value');

-- 5. eval_score rejections.
SELECT pg_temp.assert_raises($q$
  INSERT INTO eval_score (subject_id, subject_type, metric, value, eval_version, event_ts)
  VALUES ((SELECT id FROM decision WHERE data_snapshot_id = 'snap-evals'),
          'trade', 'decision_quality', 0.35, 'v1', '2026-02-05T21:00:00Z')
$q$, '23514');

SELECT pg_temp.assert_raises($q$
  INSERT INTO eval_score (subject_id, subject_type, metric, value, eval_version, event_ts)
  VALUES ((SELECT id FROM decision WHERE data_snapshot_id = 'snap-evals'),
          'Decision', 'decision_quality', 0.35, 'v1', '2026-02-05T21:00:00Z')
$q$, '23514');

SELECT pg_temp.assert_raises($q$
  INSERT INTO eval_score (subject_id, subject_type, metric, value, eval_version, event_ts)
  VALUES ((SELECT id FROM decision WHERE data_snapshot_id = 'snap-evals'),
          'decision', '', 0.35, 'v1', '2026-02-05T21:00:00Z')
$q$, '23514');

SELECT pg_temp.assert_raises($q$
  INSERT INTO eval_score (subject_id, subject_type, metric, value, eval_version, event_ts)
  VALUES ((SELECT id FROM decision WHERE data_snapshot_id = 'snap-evals'),
          'decision', 'decision_quality', 0.35, '', '2026-02-05T21:00:00Z')
$q$, '23514');

SELECT pg_temp.assert_raises($q$
  INSERT INTO eval_score (subject_id, subject_type, metric, value, eval_version, event_ts)
  VALUES (NULL, 'decision', 'decision_quality', 0.35, 'v1', '2026-02-05T21:00:00Z')
$q$, '23502');

SELECT pg_temp.assert_raises($q$
  INSERT INTO eval_score (subject_id, subject_type, metric, value, eval_version, event_ts)
  VALUES ((SELECT id FROM decision WHERE data_snapshot_id = 'snap-evals'),
          'decision', 'decision_quality', NULL, 'v1', '2026-02-05T21:00:00Z')
$q$, '23502');

SELECT pg_temp.assert_raises($q$
  INSERT INTO eval_score (subject_id, subject_type, metric, value, eval_version, event_ts)
  VALUES ((SELECT id FROM decision WHERE data_snapshot_id = 'snap-evals'),
          'decision', 'decision_quality', 0.35, 'v1', NULL)
$q$, '23502');

-- 6. Both tables are append-only (checked after rows exist).
SELECT pg_temp.assert_append_only('outcome_window');
SELECT pg_temp.assert_append_only('eval_score');
