# trend-v1

The first config of the `trend` family: the strategy the engine has traded on paper since
Phase 6. The full note, written before any backtest, is
[`docs/research/trend-v1-rationale.md`](../../docs/research/trend-v1-rationale.md); this file
restates its three parts in the form every config's note has. No backtest of this strategy
on real data had been run when either was written. Not financial advice.

## Economic rationale

Time-series momentum: an asset's own return over roughly the past year, leaving out the last
month, has tended to predict the sign of its next month's return across equity indices,
bonds and commodities (Moskowitz, Ooi and Pedersen 2012; Hurst, Ooi and Pedersen 2017). The
usual explanations are slow reaction to news followed by herding, and investors who trade
for reasons other than price. Holding an asset only while its trend is up gets out during
long declines. Volatility targeting is not a source of return; it makes position sizes mean
the same thing in calm and stormy markets.

## Expected effect

Compared with holding the eight funds in equal weight: shallower drawdowns and lower
volatility (the book targets 9% a year), in exchange for lagging after sharp reversals and
for long stretches behind a plain index fund. No claim of a higher return.

## What would change my mind

- A deflated Sharpe ratio under the gate's threshold once the whole trial history is counted.
- Results that depend on two or three of the eight funds, or on one decade.
- Live results outside the tracking band declared for the backtest replayed over the same
  period.
- Costs or slippage on paper that are well above the cost model's.
