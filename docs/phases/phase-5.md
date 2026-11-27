# Phase 5: Evals

Issue: #7. Exit criterion: a gate report is generated per strategy version.

Nothing here promotes a strategy. The gate report states, per gate, `pass`, `fail` or
`not_evaluated`, with the numbers behind it; a person reads it. A report is only "all gates
pass" when every gate is `pass`: `not_evaluated` is never read as a pass.

No real data has been through any of this: the statistics are tested against hand-computed
and published reference values, and the jobs against synthetic rows.

## Decisions taken in this phase

| Decision | Choice | Why |
|---|---|---|
| Where the statistics live | `nisse_eval::stats`: pure functions on `&[f64]`, no I/O, no clock, no dependency beyond `std` | They can be checked by hand and reused by every job. Statistics are `f64`; money stays decimal. |
| Randomness | A seeded xoshiro256** generator in `stats::rng`, seeded through SplitMix64 | The bootstrap must reproduce exactly from a stored seed; no new dependency. |
| Normal distribution | Φ by Abramowitz and Stegun 26.2.17 (error < 7.5e-8), Φ⁻¹ by Acklam's rational approximation (no refinement step) | `std` has neither; accuracy is far below what the gates need. |
| Return series per trial | Every backtest result stores its daily equity returns (`"returns"`, rounded to 10 decimal places) | The deflated Sharpe needs each trial's Sharpe; PBO needs each trial's returns over the same sessions. Rows cannot be amended later. |
| Gate thresholds | `config/eval/gates.json`, validated like the risk limits: the code refuses thresholds looser than PLAN.md | Thresholds are pre-declared and reviewed, not chosen when a result is known. |
| Holdout | The last part of the bar history, from `holdout_start` in the gates file. An ordinary backtest whose range reaches into it is refused. A holdout run is a trial marked `"holdout": true` and is refused once `holdout_max_looks` such trials exist. It must start before the holdout (its warm-up is ordinary history), and its look-count check and registration are done under a database lock. | "Queried once per candidate, logged as a trial, retired after about 10 looks." |
| Decision holdout | New append-only table `decision_holdout` (one row per decision: hidden or visible, assigned by a keyed hash, 25% hidden), a view `agent_decision` that shows only visible decisions, and a role `nisse_agent` that can read only agent views | "A random 20 to 30% of live decisions held out and never shown to agents." The assignment is fixed when the decision is first scored and cannot change. **New table, view and role: owner can veto.** |
| Outcome horizons | 1, 5 and 21 sessions, fixed in code (`OUTCOME_HORIZONS`); stored in `outcome_window.horizon` as that many days | "Horizons fixed before the trade." Changing them is a code change with a new `eval_version`. |
| Benchmark | `SPY` (gates file field `benchmark`) | The plan names no benchmark; this is the default a reader expects. **Owner to confirm.** |
| Regimes | Two, from the benchmark alone: `risk_on` when its close is above its 200-session mean, else `risk_off`; `unknown` before 200 sessions | Simple, pre-declared, uses no information from the strategy. **Owner to confirm.** |
| R unit | One R is the dollar risk the gate measured for the entry: `risk_per_trade`'s `actual_value` on the entry decision | The strategy has no stop; this is the risk figure already on record for every order. |
| Live tracking | The pure comparison is built and tested; the report says `not_evaluated` until live returns exist (Phase 6 and 9) | Nothing live to compare yet. |

## Statistics (`nisse_eval::stats`)

All take per-period (daily) simple returns `r[0..n]`. "Annualised" multiplies a per-period
Sharpe by `sqrt(periods_per_year)`. A function returns `None` when its input is too short or
its result is not finite; nothing panics and nothing returns NaN.

**Moments** (`moments.rs`). `mean = Σr/n`. `variance` is the sample variance (divide by
`n − 1`, needs `n ≥ 2`). `skewness = m3 / m2^1.5` and `kurtosis = m4 / m2²` with population
central moments `mk = Σ(r − mean)^k / n` (kurtosis is not excess: a normal sample gives about
3); both need `n ≥ 3` and `m2 > 0`. `sharpe = mean / sqrt(variance)` (per period; `None` when
the variance is zero). `sortino = mean / sqrt(Σ min(r, 0)² / n)` (`None` when there is no
negative return). `max_drawdown` of the compounded curve, as a positive fraction.
`expectancy = mean`.

**Normal** (`normal.rs`). `cdf(x)`: for `x ≥ 0`, `1 − φ(x)·(b1·t + b2·t² + b3·t³ + b4·t⁴ +
b5·t⁵)` with `t = 1 / (1 + 0.2316419·x)`, `φ(x) = exp(−x²/2) / sqrt(2π)`, `b = [0.319381530,
−0.356563782, 1.781477937, −1.821255978, 1.330274429]`; for `x < 0`, `1 − cdf(−x)`.
`inv_cdf(p)` for `0 < p < 1` (else `None`): Acklam's approximation, coefficients

```
a = [-3.969683028665376e+01,  2.209460984245205e+02, -2.759285104469687e+02,
      1.383577518672690e+02, -3.066479806614716e+01,  2.506628277459239e+00]
b = [-5.447609879822406e+01,  1.615858368580409e+02, -1.556989798598866e+02,
      6.680131188771972e+01, -1.328068155288572e+01]
c = [-7.784894002430293e-03, -3.223964580411365e-01, -2.400758277161838e+00,
     -2.549732539343734e+00,  4.374664141464968e+00,  2.938163982698783e+00]
d = [ 7.784695709041462e-03,  3.224671290700398e-01,  2.445134137142996e+00,
      3.754408661907416e+00]
p_low = 0.02425, p_high = 1 - p_low
p < p_low:   q = sqrt(-2 ln p);        x =  (((((c0 q + c1) q + c2) q + c3) q + c4) q + c5) / ((((d0 q + d1) q + d2) q + d3) q + 1)
p > p_high:  q = sqrt(-2 ln (1 - p));  x = -(((((c0 q + c1) q + c2) q + c3) q + c4) q + c5) / ((((d0 q + d1) q + d2) q + d3) q + 1)
otherwise:   q = p - 0.5, r = q²;      x =  (((((a0 r + a1) r + a2) r + a3) r + a4) r + a5) q / (((((b0 r + b1) r + b2) r + b3) r + b4) r + 1)
```

No refinement step (the cdf above is less accurate than Acklam's result). Reference values:
`cdf(0) = 0.5`, `cdf(1) = 0.8413447`, `cdf(1.96) = 0.9750021`, `cdf(-3) = 0.0013499`;
`inv_cdf(0.975) = 1.959964`, `inv_cdf(0.5) = 0`, `inv_cdf(0.001) = -3.090232`.

**Probabilistic and deflated Sharpe** (`dsr.rs`; Bailey and López de Prado 2014).

```
psr(sr, sr_star, n, skew, kurt) = Φ( (sr − sr_star)·sqrt(n − 1) / sqrt(1 − skew·sr + (kurt − 1)/4 · sr²) )
expected_max_sharpe(var_of_trial_sharpes, n_trials) =
    sqrt(var) · ( (1 − γ)·Φ⁻¹(1 − 1/N) + γ·Φ⁻¹(1 − 1/(N·e)) ),   γ = 0.5772156649015329
deflated_sharpe = psr(sr, expected_max_sharpe, n, skew, kurt)
```

All Sharpes are per period. `psr` is `None` when `n < 2` or the radicand is not positive.
`expected_max_sharpe` is `0` for `N = 1` (nothing was selected), and `None` for `N = 0` or a
negative or non-finite variance. `N` is the lifetime trial count from `trial_registry` (every
kind, every strategy; it never resets). The variance is the sample variance of the per-period
Sharpes of every backtest trial that has a stored result; with `N ≥ 2` and fewer than two
such Sharpes the deflated Sharpe is `not_evaluated`.

**Probability of backtest overfitting** (`pbo.rs`; CSCV, Bailey, Borwein, López de Prado and
Zhu). Input: `N ≥ 2` return series of equal length `T`, one per configuration, over the same
sessions. Drop the first `T mod S` rows, cut the rest into `S = 16` consecutive blocks of
equal length (at least 2 rows each, so `T ≥ 32`). For each of the `C(16, 8) = 12870` ways to
choose 8 blocks as in-sample (the other 8, in order, are out-of-sample): compute every
configuration's Sharpe in-sample and out-of-sample (a configuration whose Sharpe is `None`
counts as `−∞`); `n* =` the in-sample best (ties: the lowest index); `rank =` the
out-of-sample rank of `n*` among the `N` (1 = worst; ties share the average rank);
`ω = rank / (N + 1)`; `λ = ln(ω / (1 − ω))`. `pbo =` the fraction of combinations with
`λ ≤ 0`. `None` when `N < 2`, the lengths differ, or `T < 32`. Which trials form the matrix:
every backtest trial with a stored result whose `data_snapshot_id`, `requested_start` and
`requested_end` equal the candidate trial's and whose return series has the same length.

**Bootstrap** (`bootstrap.rs`). Stationary bootstrap (Politis and Romano): a resample of
length `n` starts at a uniform index; each next element is the following one (wrapping
round) with probability `1 − 1/L`, else a new uniform index. `interval(returns, statistic,
resamples, mean_block, level, seed)` returns the `(1 − level)/2` and `(1 + level)/2`
empirical quantiles (nearest-rank on the sorted statistics, resamples whose statistic is
`None` are dropped; `None` when fewer than half survive or `n < 2`). Defaults: 2000
resamples, mean block 21, level 0.95.

**Purged k-fold** (`cv.rs`). `folds(n, k, purge, embargo)`: fold `i` tests the contiguous
range `[i·n/k, (i+1)·n/k)`; its training set is every index outside
`[test.start − purge, test.end + embargo)`. `purge` is the label horizon (an observation whose
outcome window overlaps the test range is not trained on) and `embargo ≥` the maximum
holding period. Errors: `k < 2`, `k > n`. The strategy has no fitted parameters yet, so the
report uses the folds only to show the Sharpe of each test range.

**Tracking** (`tracking.rs`). For live and backtest daily returns over the same sessions:
`tracking_error = sqrt(variance(live − backtest)) · sqrt(periods_per_year)` and
`max_gap = max |cumulative live − cumulative backtest|` (compounded curves). Inside the band
when `tracking_error ≤ tracking_error_max` and `max_gap ≤ tracking_gap_max`.

## Gates (`config/eval/gates.json`)

| Field | Default | Accepted (else the file is refused) |
|---|---|---|
| `deflated_sharpe_min` | `0.95` | ≥ 0.95 and < 1 |
| `pbo_max` | `0.2` | > 0 and ≤ 0.2 |
| `pbo_min_configs` | `10` | ≥ 5 |
| `trial_budget` | `100` | 1 to 100 |
| `sharpe_haircut` | `0.5` | ≥ 0.5 and < 1 (the share of the backtest Sharpe removed) |
| `cv_folds` | `8` | 2 to 20 |
| `cv_embargo_sessions` | `21` | ≥ 21 (the strategy holds for up to a month) |
| `holdout_start` | `"2023-01-01"` | a date no later than 2023-01-01 (a later date would shrink the holdout) |
| `holdout_max_looks` | `10` | 1 to 10 |
| `tracking_error_max` | `0.05` | > 0 and ≤ 0.10 |
| `tracking_gap_max` | `0.05` | > 0 and ≤ 0.10 |
| `benchmark` | `"SPY"` | a non-empty symbol |
| `periods_per_year` | `252` | 200 to 366 |
| `bootstrap_seed` | `20261126` | any unsigned integer |

## Gate report (`nisse-gate-report <strategy_version_id> <gates.json>`)

For the version's newest backtest trial that has a result and is not a holdout run (the
"candidate trial"; none → the report says so and every statistical gate is `not_evaluated`):

A trial is **usable** for the statistics when its stored result has a complete return series,
was not a dirty build, records the same `holdout_start` as the report's gates file, and (for an
ordinary run) its last bar is before that date. Anything else, including results stored before
these fields existed, is counted as a trial but never used: a run that looked at the holdout
under another gates file cannot be judged as if it had not.

| Gate | Value | Pass when |
|---|---|---|
| `trial_budget` | lifetime trial count | `≤ trial_budget` |
| `deflated_sharpe` | as above, on the candidate | `≥ deflated_sharpe_min`. The trial Sharpes are those of usable trials, one per (strategy version, data snapshot, requested range), the newest. `not_evaluated` when the candidate is not usable, or, with more than one lifetime trial, when fewer than half the lifetime trials have a usable Sharpe or their variance is zero (nothing to deflate with). |
| `pbo` | as above | `≤ pbo_max`, over at least `pbo_min_configs` configurations (else `not_evaluated`) |
| `holdout` | annualised Sharpe of the version's first holdout run over the holdout sessions only | The run judged is the version's **oldest** holdout run with a stored result, whatever its inputs: the first look counts and no later look, re-run or change of costs can replace it. It is compared with the **holdout baseline**, which depends on nothing registered after the look (a run registered before the look but still running when the report is made can still become the baseline once it stores its result): the newest usable ordinary run of the version registered before that look with the holdout run's inputs (defined below) whose returns are exactly the first returns of the holdout run. That last condition is what ties the look to the run it tests: with the same inputs and the same bars before the boundary, a deterministic engine produces the same returns session for session, so any difference means the bars or an unrecorded input differed. The report's candidate must be that same experiment, else `not_evaluated` (the look tested another experiment). Two runs are the same experiment when their data snapshot, requested range, costs, symbols, limits hash, starting cash and seed are all recorded and equal; the holdout run must share all of those with the baseline except the snapshot and requested end (it covers more data), must start on the same requested date, and must have read a bar file of the same name. The candidate's returns must equal the baseline's exactly. The same definition is used wherever runs are compared: the deflated Sharpe counts one Sharpe per experiment, and the overfitting matrix takes other versions' runs only when their costs, symbols, limits, cash and seed equal the candidate's. So the last ordinary run before a holdout look is what the look tests; make the look right after the run you mean to judge. Looks `> holdout_max_looks` → `fail`. No holdout run, or no baseline → `not_evaluated`. The run ended `flat` → `fail`. Otherwise `pass` when its Sharpe `≥` the haircut of the baseline's annualised Sharpe measured from its first non-zero return (the flat warm-up would understate it), over at least 60 holdout sessions. A baseline whose Sharpe is not positive, that ended `flat` (or whose final mode is not recorded), or with fewer than 252 sessions of non-zero return → `not_evaluated`: a short or mostly idle baseline has too noisy a Sharpe to set a threshold. (A weak but long baseline is judged; the deflated Sharpe gate is what stops a weak candidate.) Because the snapshot id is a hash of the bars, a re-run after the bar history has been corrected is a different experiment: the holdout gate then stays `not_evaluated` for that version, and judging the new data takes a new strategy version and another look. A later identical ordinary re-run does not disturb a judged holdout. |
| `replay` | decisions of this version whose newest `replay_match` score is 0 | `= 0` (no scored decision → `not_evaluated`) |
| `risk_breaches` | orders of this version sent beyond the intent whose decision has a failed `risk_check` row or fewer than the eight rows the gate writes | `= 0` (no sent order → `not_evaluated`) |
| `tracking` | tracking error and gap | inside the band (`not_evaluated` without live returns) |

The report also carries, as information: the summary of the candidate trial, bootstrap
intervals for the annualised Sharpe and Sortino and for the expectancy, the Sharpe of each
purged fold, and `haircut_sharpe = (1 − sharpe_haircut) × annualised
backtest Sharpe` (floored at 0), which is the figure the limits file's `haircut_sharpe` may
not exceed; and how the candidate was run (costs, requested range, symbols, dirty build). It is printed as JSON and written as `eval_score` rows (`subject_type`
`strategy_version`, one per gate value, `eval_version` `gate-v1`). The Sharpe by regime is not in the report yet: stored return series carry no
session dates to line up with the benchmark (`outcome::regime` itself is built and tested).
Exit codes: 0 all pass, 1 a gate failed, 3 none failed but some not evaluated, 4 the report
could not be produced.

## Trade-level scores (nightly job `nisse-score`)

Run after the close; idempotent (a decision already scored for a metric and `eval_version`
is skipped). One run at a time: the job takes a database advisory lock and refuses to start
if another run holds it.

- **Decision holdout**: every decision without a `decision_holdout` row gets one. Hidden when
  the first 8 bytes of `SHA-256(salt ‖ decision id as decimal text)`, read as a big-endian
  integer, modulo 100, is below 25. The salt comes from the environment
  (`NISSE_HOLDOUT_SALT`, at least 16 bytes); without it the job refuses to run.
- **Outcome windows**: for each decision (skips included) and each horizon `h` in
  `OUTCOME_HORIZONS`, once the symbol has `h` bars after the decision's bar: `fwd_ret =
  close[t+h] / close[t] − 1`, `bench_ret` the same for the benchmark, `mfe = max(high[t+1..=t+h])
  / close[t] − 1`, `mae = min(low[t+1..=t+h]) / close[t] − 1`, where `t` is the last bar at or
  before the decision time. A decision whose symbol or benchmark has no bar at `t` is left for
  a later run. Two rules keep a decision from being scored off the wrong bar. The base bar must be the
  benchmark's newest bar too (if the benchmark has a later bar at or before the decision, the
  symbol's own bar for that session is missing). And the base bar must be less than 20 hours
  older than the decision: the engine decides at a session's close, and no two closes are
  less than 21 hours apart, so an older base bar means the decision's session is missing
  from the file for every symbol. A decision made off-session (20 hours or more after its
  bar) is therefore left unscored until the job is given the exchange calendar; the job
  counts these separately (`base_too_old`), and only once a later bar exists for the symbol
  or the benchmark: before that the decision's own bar may simply not have been ingested
  yet, and it is counted as waiting. The decision-quality score
  is not signed by action (an exit followed by a rally scores positive), so readers split it
  by action. The score (`eval_score` metric `decision_quality_<h>`) is
  `(fwd_ret − bench_ret) / (expected_vol · sqrt(h / periods_per_year))`, written only when the
  decision has an `expected_vol > 0`.
- **Execution**: scored once, three days after the decision's last order row or fill (a
  terminal state can still be overturned by reconciliation inside that window). For each
  decision with at least one order row beyond the intent:
  `shortfall_bps = side · (average fill price − arrival) / arrival · 10⁴` (side +1 for a buy,
  −1 for a sell; arrival is the decision's `close` feature, the price the decision was made
  at), `fees_bps = fees / (filled quantity · arrival) · 10⁴`, and `missed_fill_ratio =
  1 − filled / ordered` once every order of the decision is in a terminal state. Metrics
  `shortfall_bps`, `fees_bps`, `missed_fill_ratio`.
- **Exit**: round trips per symbol by first-in-first-out matching of fills. For each closed
  round trip: `realized_r = P&L / R`, `mfe_r`, `mae_r` (the excursions of the closes from the
  first bar at or after the entry up to the exit, times the quantity, over R) and `capture = realized P&L / (MFE in dollars)`
  when the MFE is positive. Stored against the entry decision (metrics `realized_r`, `mfe_r`,
  `mae_r`, `capture`).

## Tasks

| Task | What | Files | Order |
|---|---|---|---|
| F | Foundation (lead): this doc, gates file and parser, stats stubs, signatures | many | 0 |
| ST1 | Normal cdf and inverse | `crates/eval/src/stats/normal.rs` | 1 |
| ST2 | Moments, Sharpe, Sortino, drawdown | `crates/eval/src/stats/moments.rs` | 1 |
| ST3 | Seeded generator and stationary bootstrap | `crates/eval/src/stats/rng.rs`, `bootstrap.rs` | 1 |
| ST4 | Purged k-fold and tracking | `crates/eval/src/stats/cv.rs`, `tracking.rs` | 1 |
| DB1 | `decision_holdout`, `agent_decision`, role `nisse_agent` | `migrations/0011_decision_holdout.sql`, tests | 1 |
| ST5 | Probabilistic and deflated Sharpe | `crates/eval/src/stats/dsr.rs` | 2 (ST1, ST2) |
| ST6 | PBO by CSCV | `crates/eval/src/stats/pbo.rs` | 2 (ST2) |
| OW1 | Outcome windows and regimes (pure) | `crates/eval/src/outcome.rs` | 2 (ST2) |
| EX1 | Shortfall and round trips (pure) | `crates/eval/src/execution.rs` | 2 |
| BT1 | Returns in backtest results; holdout rule in the backtest job | `crates/eval/src/backtest_job.rs`, tests | 2 |
| DB2 | Store modules: outcome windows, holdout rows, the reads the jobs need | `crates/store/src/tables/*`, tests | 3 (DB1) |
| JB1 | `nisse-score` job | `crates/eval/src/score_job.rs`, binary, tests | 4 |
| GR1 | Gate report and `nisse-gate-report` | `crates/eval/src/gate_report.rs`, binary, tests | 4 |
