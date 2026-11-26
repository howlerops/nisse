//! One module per store table. All SQL for a table lives in its module and nowhere else.
//!
//! Every function takes `&impl GenericClient`, so it runs equally on a connection or inside
//! a transaction. Every statement is an `INSERT` or a `SELECT`: store tables are append-only.

pub mod decision;
pub mod eval_score;
pub mod fill;
pub mod market_snapshot;
pub mod order_event;
pub mod position_event;
pub mod risk_check;
pub mod strategy_version;
pub mod trial_registry;
pub mod trial_result;
