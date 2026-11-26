# Runbook: Alpaca paper tests

## What they are for

The Phase 1 exit criterion (issue #3) is that an order round trip and restart reconciliation
pass against Alpaca paper. These tests are the only place that happens. They also answer one
open question: whether Alpaca accepts a fractional day limit order (issue #12).

## Who runs them

The owner, on the engine host. Broker credentials live only there. Agents and CI never hold
them, so agents cannot run these tests.

## Prerequisites

- An Alpaca PAPER account with an API key pair. Export the two variables in the shell only:
  `APCA_API_KEY_ID` and `APCA_API_SECRET_KEY`. Never write them to a file in the repo.
- The pinned Rust toolchain. It installs itself on the first `cargo` call.

## Command

```
cargo test -p nisse-alpaca-adapter --test paper -- --ignored --test-threads=1 --nocapture
```

`--nocapture` is needed to see the `FRACTIONAL LIMIT:` line.

## The four tests

1. `order_round_trip`: places a day limit buy of 1 SPY at 1.00, checks that the intent log
   holds it and that Alpaca finds it by client order id, cancels it, and syncs until the
   tracker shows `Canceled`. Pass: every check holds and the order ends `Canceled`.
2. `restart_mid_order_reconciles`: places the same order, drops the tracker, rebuilds a
   tracker from the intent alone, and syncs. Pass: the rebuilt tracker adopts the broker's
   venue order id and a non-terminal state, reports findings, and then the order is cancelled
   and ends `Canceled`.
3. `fractional_limit_order_probe`: places a day limit buy of 0.5 SPY at 1.00. The test passes
   either way. It prints `FRACTIONAL LIMIT: accepted, state ...` (then cancels) or
   `FRACTIONAL LIMIT: rejected: ...`. A panic means the outcome was unknown, which is a failed
   probe, not an answer.
4. `calendar_has_a_half_day`: fetches the current year's calendar. Pass: more than 240
   sessions and at least one half-day.

## Safety

- The paper endpoint is hard-coded, and `paper_config()` asserts it before any request.
- Every order is a day limit buy at 1.00, far below SPY, so it cannot fill. Each order is
  cancelled before its test ends, even when a check fails.
- The tests can run with the market closed: a day limit order is accepted and queued.
- If a run is interrupted, or a probe panics, cancel any leftover orders from the Alpaca paper
  dashboard. Check for open orders on the paper account before the next run.

## Afterwards

- In issue #3, tick the two "Integration test" boxes and the fractional-limit box, once the
  tests pass.
- In issue #12 (Open questions), record the `FRACTIONAL LIMIT:` line that was printed.
