//! What a simulated fill costs: slippage against the reference price, and fees.

use nisse_core::nautilus::OrderSide;
use rust_decimal::{Decimal, RoundingStrategy};

/// A simple, deliberately pessimistic cost model for backtests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CostModel {
    /// Slippage against the reference price, in basis points, always against the order: a buy
    /// fills higher, a sell lower. Covers half the spread plus impact.
    pub slippage_bps: Decimal,
    /// Fee per share traded, in dollars (regulatory fees; the broker's commission is zero).
    pub fee_per_share: Decimal,
}

/// A fill priced by the model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PricedFill {
    /// The fill price, rounded to 6 decimal places away from the order's favour.
    pub px: Decimal,
    /// Total fee for the quantity, rounded up to the cent. Never negative.
    pub fee: Decimal,
}

/// Decimal places kept on a fill price.
const PRICE_DP: u32 = 6;
/// Decimal places kept on a fee (cents).
const FEE_DP: u32 = 2;

impl CostModel {
    /// Errors with `InvalidConfig` when either figure is negative.
    pub fn new(slippage_bps: Decimal, fee_per_share: Decimal) -> Result<Self, crate::EngineError> {
        if slippage_bps < Decimal::ZERO {
            return Err(crate::EngineError::InvalidConfig(
                "slippage_bps must not be negative".to_owned(),
            ));
        }
        if fee_per_share < Decimal::ZERO {
            return Err(crate::EngineError::InvalidConfig(
                "fee_per_share must not be negative".to_owned(),
            ));
        }
        Ok(Self {
            slippage_bps,
            fee_per_share,
        })
    }

    /// Prices a fill of `qty` shares at `reference_px` (both positive).
    #[must_use]
    pub fn price(&self, side: OrderSide, reference_px: Decimal, qty: Decimal) -> PricedFill {
        let px = self
            .checked_px(side, reference_px)
            .unwrap_or(reference_px)
            .normalize();
        let fee = self.checked_fee(qty).unwrap_or(Decimal::ZERO).normalize();
        PricedFill { px, fee }
    }

    /// `reference_px` moved against the order by the slippage, rounded in the order's
    /// disfavour. `None` on overflow.
    fn checked_px(&self, side: OrderSide, reference_px: Decimal) -> Option<Decimal> {
        let rate = self.slippage_bps.checked_div(Decimal::from(10_000))?;
        let (factor, strategy) = match side {
            OrderSide::Buy => (
                Decimal::ONE.checked_add(rate)?,
                RoundingStrategy::AwayFromZero,
            ),
            OrderSide::Sell => (Decimal::ONE.checked_sub(rate)?, RoundingStrategy::ToZero),
        };
        let raw = reference_px.checked_mul(factor)?;
        // Slippage above 10,000 bps would push a sell price below zero; floor it at zero.
        if raw < Decimal::ZERO {
            return Some(Decimal::ZERO);
        }
        Some(raw.round_dp_with_strategy(PRICE_DP, strategy))
    }

    /// `qty * fee_per_share` rounded up to the cent. `None` on overflow.
    fn checked_fee(&self, qty: Decimal) -> Option<Decimal> {
        let raw = qty.checked_mul(self.fee_per_share)?;
        Some(raw.round_dp_with_strategy(FEE_DP, RoundingStrategy::AwayFromZero))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "tests build decimals from literals")]
mod tests {
    use super::*;
    use crate::EngineError;

    fn d(s: &str) -> Decimal {
        s.parse().unwrap()
    }

    fn model(bps: &str, fee: &str) -> CostModel {
        CostModel::new(d(bps), d(fee)).unwrap()
    }

    #[test]
    fn buy_and_sell_move_against_the_order_by_slippage() {
        let m = model("5", "0");
        assert_eq!(m.price(OrderSide::Buy, d("100"), d("1")).px, d("100.05"));
        assert_eq!(m.price(OrderSide::Sell, d("100"), d("1")).px, d("99.95"));
    }

    #[test]
    fn zero_slippage_gives_the_reference_price_on_both_sides() {
        let m = model("0", "0");
        assert_eq!(m.price(OrderSide::Buy, d("123.45"), d("1")).px, d("123.45"));
        assert_eq!(
            m.price(OrderSide::Sell, d("123.45"), d("1")).px,
            d("123.45")
        );
    }

    #[test]
    fn rounding_never_favours_the_order() {
        // Unrounded at 1 bp: buy = 10.0000033 * 1.0001 = 10.00100330033 -> up to 10.001004;
        // sell = 10.0000033 * 0.9999 = 9.99900329967 -> down to 9.999003.
        let m = model("1", "0");
        let buy = m.price(OrderSide::Buy, d("10.0000033"), d("1")).px;
        let sell = m.price(OrderSide::Sell, d("10.0000033"), d("1")).px;
        assert_eq!(buy, d("10.001004"));
        assert_eq!(sell, d("9.999003"));
        assert!(buy > d("10.0000033"));
        assert!(sell < d("10.0000033"));
    }

    #[test]
    fn fee_rounds_up_to_the_cent() {
        assert_eq!(
            model("0", "0.0001")
                .price(OrderSide::Buy, d("50"), d("100"))
                .fee,
            d("0.01")
        );
        assert_eq!(
            model("0", "0.0001")
                .price(OrderSide::Buy, d("50"), d("150"))
                .fee,
            d("0.02")
        );
        assert_eq!(
            model("0", "0.01")
                .price(OrderSide::Buy, d("50"), d("0.5"))
                .fee,
            d("0.01")
        );
    }

    #[test]
    fn zero_fee_rate_gives_zero_fee() {
        let fill = model("5", "0").price(OrderSide::Sell, d("50"), d("1000"));
        assert_eq!(fill.fee, Decimal::ZERO);
        assert!(fill.fee.is_zero());
    }

    #[test]
    fn negative_inputs_are_invalid_config() {
        let neg_slip = CostModel::new(d("-1"), d("0"));
        assert!(
            matches!(neg_slip, Err(EngineError::InvalidConfig(ref m)) if m.contains("slippage"))
        );
        let neg_fee = CostModel::new(d("0"), d("-0.0001"));
        assert!(matches!(neg_fee, Err(EngineError::InvalidConfig(ref m)) if m.contains("fee")));
    }

    #[test]
    fn zero_inputs_are_valid() {
        assert_eq!(
            CostModel::new(d("0"), d("0")).unwrap(),
            CostModel {
                slippage_bps: Decimal::ZERO,
                fee_per_share: Decimal::ZERO,
            }
        );
    }

    #[test]
    fn fractional_qty_and_many_decimals_give_six_dp_prices() {
        // 123.456789123 * 1.0001 = 123.4691348019123 -> 123.469135 (up)
        // 123.456789123 * 0.9999 = 123.4444434440877 -> 123.444443 (down)
        let m = model("1", "0.01");
        let buy = m.price(OrderSide::Buy, d("123.456789123"), d("0.333333"));
        let sell = m.price(OrderSide::Sell, d("123.456789123"), d("0.333333"));
        assert_eq!(buy.px, d("123.469135"));
        assert_eq!(sell.px, d("123.444443"));
        assert!(buy.px.scale() <= PRICE_DP);
        assert!(sell.px.scale() <= PRICE_DP);
        assert_eq!(buy.fee, d("0.01"));
    }

    #[test]
    fn price_is_independent_of_qty_and_fee_of_side() {
        let m = model("7", "0.0035");
        let small = m.price(OrderSide::Buy, d("42.5"), d("1"));
        let large = m.price(OrderSide::Buy, d("42.5"), d("10000"));
        assert_eq!(small.px, large.px);

        let buy = m.price(OrderSide::Buy, d("42.5"), d("300"));
        let sell = m.price(OrderSide::Sell, d("42.5"), d("300"));
        assert_eq!(buy.fee, sell.fee);
        assert_eq!(buy.fee, d("1.05"));
    }

    #[test]
    fn slippage_above_ten_thousand_bps_floors_sell_at_zero() {
        let m = model("20000", "0");
        assert_eq!(m.price(OrderSide::Sell, d("100"), d("1")).px, Decimal::ZERO);
        assert_eq!(m.price(OrderSide::Buy, d("100"), d("1")).px, d("300"));
    }

    #[test]
    fn overflow_falls_back_to_reference_price_and_zero_fee() {
        // A 2.0 fee rate on MAX shares overflows the fee; 1 bp on MAX overflows the price.
        let m = model("1", "2");
        let fill = m.price(OrderSide::Buy, Decimal::MAX, Decimal::MAX);
        assert_eq!(fill.px, Decimal::MAX);
        assert_eq!(fill.fee, Decimal::ZERO);
    }

    #[test]
    fn outputs_are_normalized() {
        let fill = model("0", "0").price(OrderSide::Buy, d("100.050000"), d("1"));
        assert_eq!(fill.px.to_string(), "100.05");
        assert_eq!(
            model("0", "0.5")
                .price(OrderSide::Buy, d("1"), d("2"))
                .fee
                .to_string(),
            "1"
        );
    }
}
