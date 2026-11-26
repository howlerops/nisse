-- The store has exactly the tables in PLAN.md (plus the migration ledger). A new table must be
-- added here on purpose, alongside PLAN.md; a dropped or renamed one fails here.

SELECT pg_temp.assert_true(
  (SELECT array_agg(c.relname::text ORDER BY c.relname)
   FROM pg_class c
   WHERE c.relnamespace = 'public'::regnamespace AND c.relkind IN ('r', 'p'))
  = ARRAY[
      'decision', 'eval_score', 'fill', 'market_snapshot', 'order_event', 'outcome_window',
      'position_event', 'risk_check', 'schema_migration', 'strategy_version', 'trial_registry'
    ],
  'the set of store tables differs from PLAN.md');
