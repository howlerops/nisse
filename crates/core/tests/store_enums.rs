//! Locks the text form of the store enums. These strings are stored in Postgres and fixed by
//! CHECK constraints, so the expected values are literal lists, not derived from the enums.

use std::{fmt::Debug, fs, io, path::Path, str::FromStr};

use nisse_core::{Action, CoreError, OrderState, SubjectType, TrialKind};
use strum::VariantArray;

/// The stored string of every variant of `T`, in declaration order.
fn strings<T: VariantArray + Copy + Into<&'static str>>() -> Vec<&'static str> {
    T::VARIANTS.iter().map(|v| (*v).into()).collect()
}

#[test]
fn action_strings_are_stable() {
    assert_eq!(strings::<Action>(), ["enter", "exit", "hold", "skip"]);
}

#[test]
fn trial_kind_strings_are_stable() {
    assert_eq!(strings::<TrialKind>(), ["backtest", "shadow", "live"]);
}

#[test]
fn subject_type_strings_are_stable() {
    assert_eq!(strings::<SubjectType>(), ["decision", "strategy_version"]);
}

/// Every variant of `T` parses back from its `Display` text, which equals its stored string.
fn assert_round_trips<T>()
where
    T: VariantArray + Copy + PartialEq + Into<&'static str> + ToString + FromStr + Debug,
{
    for v in T::VARIANTS {
        let text = v.to_string();
        assert_eq!(text.parse::<T>().ok(), Some(*v));
        assert_eq!(text, (*v).into());
    }
}

#[test]
fn enums_round_trip_through_text() {
    assert_round_trips::<Action>();
    assert_round_trips::<TrialKind>();
    assert_round_trips::<SubjectType>();
}

#[test]
fn unknown_and_wrong_case_values_are_rejected() {
    assert!("buy".parse::<Action>().is_err());
    assert!("Enter".parse::<Action>().is_err());
    assert!("".parse::<TrialKind>().is_err());
    assert!("paper".parse::<TrialKind>().is_err());

    let err = CoreError::from("buy".parse::<Action>().unwrap_err());
    assert!(matches!(err, CoreError::UnknownVariant(_)));
}

/// The SQL `CHECK` list in a migration names exactly the enum's strings, in order. Fails when a
/// variant is added or renamed on one side only.
fn assert_check_matches<T>(migration: &str, column: &str) -> io::Result<()>
where
    T: VariantArray + Copy + Into<&'static str>,
{
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../migrations")
        .join(migration);
    let sql = fs::read_to_string(&path)?;
    let list = strings::<T>()
        .iter()
        .map(|s| format!("'{s}'"))
        .collect::<Vec<_>>()
        .join(", ");
    let want = format!("CHECK ({column} IN ({list}))");
    assert!(sql.contains(&want), "{migration} must contain: {want}");
    Ok(())
}

#[test]
fn sql_check_lists_match_the_enums() -> io::Result<()> {
    assert_check_matches::<Action>("0003_decision.sql", "action")?;
    assert_check_matches::<TrialKind>("0002_strategy_trial.sql", "kind")?;
    assert_check_matches::<SubjectType>("0005_evals.sql", "subject_type")?;
    assert_check_matches::<OrderState>("0006_order_columns.sql", "state")
}
