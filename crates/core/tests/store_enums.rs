//! Locks the text form of the store enums. These strings are stored in Postgres and fixed by
//! CHECK constraints, so the expected values are literal lists, not derived from the enums.

use nisse_core::{Action, CoreError, SubjectType, TrialKind};
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

#[test]
fn enums_round_trip_through_text() {
    for v in Action::VARIANTS {
        let text = v.to_string();
        assert_eq!(text.parse::<Action>().ok(), Some(*v));
        assert_eq!(text, <&'static str>::from(*v));
    }
    for v in TrialKind::VARIANTS {
        let text = v.to_string();
        assert_eq!(text.parse::<TrialKind>().ok(), Some(*v));
        assert_eq!(text, <&'static str>::from(*v));
    }
    for v in SubjectType::VARIANTS {
        let text = v.to_string();
        assert_eq!(text.parse::<SubjectType>().ok(), Some(*v));
        assert_eq!(text, <&'static str>::from(*v));
    }
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
