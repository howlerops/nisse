# Trend v1: rationale, written before any backtest

Status: **proposal**. No backtest of this strategy has been run. This note states what the
strategy is, why it might work, what would count as it not working, and the parameters, so
that none of them can be chosen after seeing results. Not financial advice.

## What it is

Once a month, at the close of the last trading session, for each asset in a small set of
broad ETFs:

1. Hold the asset only if its own return over roughly the past year, leaving out the most
   recent month, is positive. Otherwise hold none of it (the cash sits idle).
2. Size each held asset so that it contributes an equal share of a 9% annual portfolio
   volatility target, using its own volatility over the past 60 sessions, capped at 12.5% of
   equity per asset (one eighth, so the eight caps add up to exactly the account). There is
   no leverage and no shorting.
3. Trade the difference at the next session's open, unless the change is too small to bother.

## Why it might work

Time-series momentum, the tendency of an asset's own trailing return to predict the sign of
its next month's return, has been documented across equity indices, bonds, currencies and
commodities over more than a century (Moskowitz, Ooi and Pedersen, "Time Series Momentum",
2012; Hurst, Ooi and Pedersen, "A Century of Evidence on Trend-Following Investing", 2017).
The usual explanations are behavioural (slow reaction to news, then herding) and structural
(hedgers and risk-targeting investors trading for reasons other than price). The practical
appeal for a small account is different: the rule gets out of an asset during long declines,
which has historically cut the depth of drawdowns more than it has raised returns.

Volatility targeting is not a source of return. It is there so that the position sizes mean
the same thing in calm and stormy markets, and so that the portfolio's risk is known in
advance, which the risk controls in PLAN.md assume.

## Why it might not

- The effect is weaker and less reliable in the decade since it became widely traded.
- Monthly trend following loses money in sideways markets that reverse often ("whipsaw"),
  sometimes for years. Long stretches behind a plain index fund are the expected case.
- Eight assets is a small sample; two or three of them will drive any backtest result.
- A backtest on ETF history only covers about two decades and one long bond bull market.
- Costs, taxes on short-term gains, and the gap between the close that triggers a decision
  and the open that fills it all work against it.

## Universe

Chosen for breadth of asset class, long history and liquidity, before looking at any return:

| Symbol | Exposure |
|---|---|
| SPY | US large-cap equities |
| EFA | Developed markets outside the US |
| EEM | Emerging market equities |
| IEF | US Treasuries, 7 to 10 years |
| TLT | US Treasuries, 20 years and longer |
| GLD | Gold |
| DBC | Broad commodities |
| VNQ | US real estate investment trusts |

## Parameters, fixed now

| Parameter | Value | Why this value |
|---|---|---|
| Lookback | 252 sessions | The 12-month horizon used throughout the literature |
| Skip | 21 sessions | Leaves out the most recent month, where returns tend to reverse |
| Volatility window | 60 sessions | About a quarter: responsive without chasing single weeks |
| Portfolio volatility target | 9% a year | Middle of the 8 to 10% range in PLAN.md |
| Cap per asset | 12.5% of equity | One eighth: the portfolio cannot exceed 100% of equity even if all eight assets are held at the cap |
| Minimum trade | 25 dollars | Skips changes that are noise at this account size |
| Rebalance | Last session of each month | Monthly, as planned; swing holding periods only |

These are the conventional choices, not fitted ones. Changing any of them later is a new
strategy version and a new trial.

## What I expect, stated first

- A net Sharpe ratio between 0.3 and 0.6 over the available history, as PLAN.md's realistic
  good outcome says, with a maximum drawdown clearly smaller than SPY's over the same span.
- Lower return than SPY over the last fifteen years.
- If the backtest shows a Sharpe ratio above about 1, the most likely explanation is a bug
  or look-ahead, not a discovery, and it should be treated that way.

## What would count against it

- A deflated Sharpe ratio below the 0.95 gate once the lifetime trial count is applied.
- Results that depend on one asset or one decade.
- A live or paper track that leaves the pre-declared band around the backtest replayed over
  the same period.

## Open decisions for the owner

- **Market data feed.** Alpaca's free feed builds daily bars from one exchange's trades, so
  its closes and volumes can differ from the consolidated tape; the paid feed does not have
  that problem. The code takes the feed as configuration. Which one the backtest uses is the
  open question in PLAN.md and should be settled before the first real run.
- **This note itself.** Merging it is the approval of the universe and parameters above.
