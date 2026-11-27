//! Acceptance tests for research configs, notes and the proposal check. The checks read only
//! small directory trees that each test builds under a unique temporary directory, removed
//! when the test ends.

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "integration test")]

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use chrono::NaiveDate;
use nisse_eval::EvalError;
use nisse_eval::proposal::{
    Change, ResearchConfig, budget_conflicts, check_note, check_proposal, is_plain_path,
    parse_changes, read_regular,
};
use serde_json::{Value, json};

/// "Today" for every test.
fn today() -> NaiveDate {
    date(2026, 11, 27)
}

fn date(year: i32, month: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(year, month, day).unwrap()
}

/// A date `days` after 2026-10-25, so that `day(30)` and `day(31)` are within the week before
/// [`today`] (a proposal's `created` may be at most seven days old).
fn day(days: i64) -> NaiveDate {
    date(2026, 10, 25) + chrono::TimeDelta::days(days)
}

const VALID_NOTE: &str = "# note\n\n## Economic rationale\n\nA reason that is written here.\n\n\
## Expected effect\n\nAn effect, stated before any result.\n\n\
## What would change my mind\n\nA result that would make me stop.\n";

const EFFECT: &str = "A diversified trend book with a long enough effect text to pass the rule.";

/// A temporary directory tree, removed on drop.
struct Tree {
    root: PathBuf,
}

impl Tree {
    fn new() -> Self {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let root = std::env::temp_dir().join(format!("nisse-proposal-{}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        Self { root }
    }

    fn path(&self) -> &Path {
        &self.root
    }

    fn write(&self, rel: &str, text: &str) {
        let file = self.root.join(rel);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(file, text).unwrap();
    }

    /// Writes a config file `research/configs/<name>.json` and its note.
    fn put(&self, config: &Value) {
        let name = config["name"].as_str().unwrap();
        self.write(
            &format!("research/configs/{name}.json"),
            &config.to_string(),
        );
        self.write(&format!("research/notes/{name}.md"), VALID_NOTE);
    }

    /// Makes `rel` a symbolic link to `target` (which is not followed by the checks).
    #[cfg(unix)]
    fn link(&self, rel: &str, target: &str) {
        let file = self.root.join(rel);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(target, file).unwrap();
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// A valid config as JSON, with the fields the tests vary.
fn config(name: &str, family: &str, created: NaiveDate, parent: Value) -> Value {
    json!({
        "name": name,
        "family": family,
        "created": created.format("%Y-%m-%d").to_string(),
        "parent": parent,
        "strategy": {
            "strategy": "trend_vol_target",
            "lookback_days": 252,
            "skip_days": 21,
            "vol_window": 60,
            "target_vol": "0.09",
            "max_weight": "0.125",
            "min_trade_notional": "25",
            "sessions_per_year": 252
        },
        "symbols": ["SPY", "TLT"],
        "benchmark": "SPY",
        "rationale": format!("research/notes/{name}.md"),
        "expected_effect": EFFECT
    })
}

fn added(path: &str) -> Change {
    Change::Added(path.to_owned())
}

fn modified(path: &str) -> Change {
    Change::Modified(path.to_owned())
}

/// The change list for adding one config and its note.
fn added_config(name: &str) -> Vec<Change> {
    vec![
        added(&format!("research/configs/{name}.json")),
        added(&format!("research/notes/{name}.md")),
    ]
}

/// Runs the check with `base` and `head` as the two trees, for an agent pull request.
fn run(changes: &[Change], base: &Tree, head: &Tree) -> Vec<String> {
    check_proposal(changes, true, base.path(), head.path(), today())
}

/// Runs the check as a person's own pull request (not an agent's): rules 1 and 2 do not bind.
fn run_person(changes: &[Change], base: &Tree, head: &Tree) -> Vec<String> {
    check_proposal(changes, false, base.path(), head.path(), today())
}

/// Two trees that start with the same base contents: `base_configs` written to both.
fn trees(base_configs: &[Value]) -> (Tree, Tree) {
    let base = Tree::new();
    let head = Tree::new();
    for c in base_configs {
        base.put(c);
        head.put(c);
    }
    (base, head)
}

// ---- the repository's own files ---------------------------------------------------------------

#[test]
fn the_repository_config_and_note_are_valid() {
    let config = ResearchConfig::parse(include_str!("../../../research/configs/trend-v1.json"))
        .expect("trend-v1 parses");
    assert_eq!(config.name, "trend-v1");
    check_note(include_str!("../../../research/notes/trend-v1.md")).expect("trend-v1 note passes");
}

// ---- ResearchConfig::parse -------------------------------------------------------------------

fn parse_error(value: &Value) -> String {
    ResearchConfig::parse(&value.to_string())
        .expect_err("config should be refused")
        .to_string()
}

#[test]
fn parse_accepts_a_valid_config_and_sorts_symbols() {
    let mut value = config("trend-v1", "trend", day(0), Value::Null);
    value["symbols"] = json!(["TLT", "SPY", "EEM"]);
    let parsed = ResearchConfig::parse(&value.to_string()).unwrap();
    assert_eq!(parsed.symbols, vec!["EEM", "SPY", "TLT"]);
    assert_eq!(parsed.parent, None);
    assert_eq!(parsed.created, day(0));
}

#[test]
fn parse_refuses_an_unknown_field() {
    let mut value = config("trend-v1", "trend", day(0), Value::Null);
    value["extra"] = json!(1);
    assert!(parse_error(&value).contains("extra"));
}

#[test]
fn parse_refuses_a_missing_parent() {
    let mut value = config("trend-v1", "trend", day(0), Value::Null);
    value.as_object_mut().unwrap().remove("parent");
    assert!(parse_error(&value).contains("parent"));
}

#[test]
fn parse_refuses_a_bad_name() {
    for bad in ["Trend", "trend--v1", "-trend", "trend-", "", "trend_v1"] {
        let value = config(bad, "trend", day(0), Value::Null);
        assert!(parse_error(&value).contains("name"), "name {bad:?}");
    }
    let long = "a".repeat(49);
    let value = config(&long, "trend", day(0), Value::Null);
    assert!(parse_error(&value).contains("name"));
}

#[test]
fn parse_refuses_a_name_with_a_double_hyphen_even_if_short() {
    let value = config("a--b", "trend", day(0), Value::Null);
    assert!(parse_error(&value).contains("name"));
}

#[test]
fn parse_refuses_a_bad_family() {
    let value = config("trend-v1", &"f".repeat(33), day(0), Value::Null);
    assert!(parse_error(&value).contains("family"));
    let value = config("trend-v1", "Trend", day(0), Value::Null);
    assert!(parse_error(&value).contains("family"));
}

#[test]
fn parse_refuses_a_bad_date() {
    for bad in [
        "2026-1-05",
        "2026/01/05",
        "2026-02-30",
        "20260105",
        "2026-01-05T",
    ] {
        let mut value = config("trend-v1", "trend", day(0), Value::Null);
        value["created"] = json!(bad);
        assert!(parse_error(&value).contains("created"), "date {bad:?}");
    }
}

#[test]
fn parse_refuses_a_parent_equal_to_the_name() {
    let value = config("trend-v1", "trend", day(0), json!("trend-v1"));
    assert!(parse_error(&value).contains("parent"));
}

#[test]
fn parse_refuses_a_parent_that_is_not_a_name() {
    let value = config("trend-v1", "trend", day(0), json!("Trend_V0"));
    assert!(parse_error(&value).contains("parent"));
    let value = config("trend-v1", "trend", day(0), json!(3));
    assert!(parse_error(&value).contains("parent"));
}

#[test]
fn parse_refuses_an_unknown_strategy() {
    let mut value = config("trend-v1", "trend", day(0), Value::Null);
    value["strategy"] = json!({"strategy": "no_such_strategy"});
    assert!(parse_error(&value).contains("strategy"));
    value["strategy"] = json!("trend_vol_target");
    assert!(parse_error(&value).contains("strategy"));
}

#[test]
fn parse_refuses_empty_symbols() {
    let mut value = config("trend-v1", "trend", day(0), Value::Null);
    value["symbols"] = json!([]);
    assert!(parse_error(&value).contains("symbols"));
}

#[test]
fn parse_refuses_a_duplicate_symbol() {
    let mut value = config("trend-v1", "trend", day(0), Value::Null);
    value["symbols"] = json!(["SPY", "TLT", "SPY"]);
    assert!(parse_error(&value).contains("SPY"));
}

#[test]
fn parse_refuses_a_lower_case_symbol() {
    let mut value = config("trend-v1", "trend", day(0), Value::Null);
    value["symbols"] = json!(["spy"]);
    assert!(parse_error(&value).contains("symbols"));
}

#[test]
fn parse_refuses_a_symbol_that_is_too_long_or_has_bad_characters() {
    let mut value = config("trend-v1", "trend", day(0), Value::Null);
    value["symbols"] = json!(["ABCDEFGHIJKLM"]);
    assert!(parse_error(&value).contains("symbols"));
    value["symbols"] = json!(["SP-Y"]);
    assert!(parse_error(&value).contains("symbols"));
    value["symbols"] = json!(["SPY"]);
    value["benchmark"] = json!("spy");
    assert!(parse_error(&value).contains("benchmark"));
}

#[test]
fn parse_refuses_a_wrong_rationale_path() {
    let mut value = config("trend-v1", "trend", day(0), Value::Null);
    value["rationale"] = json!("research/notes/other.md");
    assert!(parse_error(&value).contains("rationale"));
}

#[test]
fn parse_refuses_a_short_expected_effect() {
    let mut value = config("trend-v1", "trend", day(0), Value::Null);
    value["expected_effect"] = json!("   too short    ");
    assert!(parse_error(&value).contains("expected_effect"));
}

#[test]
fn parse_refuses_invalid_json() {
    assert!(ResearchConfig::parse("{").is_err());
}

// ---- check_note ------------------------------------------------------------------------------

#[test]
fn check_note_accepts_the_three_headings_with_text() {
    check_note(VALID_NOTE).unwrap();
}

#[test]
fn check_note_refuses_a_missing_heading() {
    let text = VALID_NOTE.replace("## Expected effect", "## Effect");
    assert!(check_note(&text).is_err());
}

#[test]
fn check_note_refuses_headings_out_of_order() {
    let text = "## What would change my mind\n\nx\n\n## Expected effect\n\ny\n\n\
                ## Economic rationale\n\nz\n";
    assert!(check_note(text).is_err());
}

#[test]
fn check_note_refuses_a_heading_with_no_text_under_it() {
    let text = VALID_NOTE.replace(
        "## Expected effect\n\nAn effect, stated before any result.\n\n",
        "## Expected effect\n\n",
    );
    assert!(check_note(&text).is_err());
    // Only a nested heading does not count as text.
    let text = VALID_NOTE.replace(
        "## Expected effect\n\nAn effect, stated before any result.\n\n",
        "## Expected effect\n### detail\n\n",
    );
    assert!(check_note(&text).is_err());
}

#[test]
fn check_note_refuses_the_last_heading_with_no_text() {
    let text = VALID_NOTE.replace("A result that would make me stop.\n", "");
    assert!(check_note(&text).is_err());
}

// ---- parse_changes ---------------------------------------------------------------------------

#[test]
fn parse_changes_reads_nul_separated_records() {
    let changes = parse_changes("A\0a\0M\0b\0D\0c\0").unwrap();
    assert_eq!(
        changes,
        vec![
            Change::Added("a".into()),
            Change::Modified("b".into()),
            Change::Deleted("c".into()),
        ]
    );
}

#[test]
fn parse_changes_reads_each_status() {
    let text = "A\0a.json\0M\0b.json\0D\0c.json\0R100\0old.md\0new.md\0T\0d\0";
    let changes = parse_changes(text).unwrap();
    assert_eq!(
        changes,
        vec![
            Change::Added("a.json".into()),
            Change::Modified("b.json".into()),
            Change::Deleted("c.json".into()),
            Change::Renamed {
                from: "old.md".into(),
                to: "new.md".into()
            },
            Change::Other {
                status: "T".into(),
                path: "d".into()
            },
        ]
    );
}

#[test]
fn parse_changes_reads_a_rename_record() {
    let changes = parse_changes("R087\0research/notes/a.md\0research/notes/b.md\0").unwrap();
    assert_eq!(
        changes,
        vec![Change::Renamed {
            from: "research/notes/a.md".into(),
            to: "research/notes/b.md".into()
        }]
    );
}

#[test]
fn parse_changes_keeps_both_paths_of_a_copy() {
    let changes = parse_changes("C75\0research/configs/a.json\0research/configs/b.json\0").unwrap();
    assert_eq!(
        changes,
        vec![Change::Copied {
            from: "research/configs/a.json".into(),
            to: "research/configs/b.json".into()
        }]
    );
    // The source of a copy is checked too: a copy out of research/ is refused.
    let (base, head) = trees(&[]);
    let failures = run(
        &[Change::Copied {
            from: "crates/x.rs".into(),
            to: "research/configs/b.json".into(),
        }],
        &base,
        &head,
    );
    assert!(
        failures.iter().any(|f| f.contains("crates/x.rs")),
        "{failures:?}"
    );
}

#[test]
fn parse_changes_reads_a_type_change() {
    assert_eq!(
        parse_changes("T\0d\0").unwrap(),
        vec![Change::Other {
            status: "T".into(),
            path: "d".into()
        }]
    );
}

#[test]
fn parse_changes_empty_input_is_no_changes() {
    assert!(parse_changes("").unwrap().is_empty());
}

#[test]
fn parse_changes_accepts_a_missing_trailing_nul() {
    assert_eq!(
        parse_changes("M\0a").unwrap(),
        vec![Change::Modified("a".into())]
    );
}

#[test]
fn parse_changes_refuses_a_missing_path() {
    assert!(parse_changes("A\0").is_err());
    assert!(parse_changes("A").is_err());
    assert!(parse_changes("A\0a\0M\0").is_err());
    assert!(parse_changes("R100\0old\0").is_err());
    assert!(parse_changes("C50\0only\0").is_err());
}

#[test]
fn parse_changes_refuses_an_empty_path() {
    assert!(parse_changes("A\0\0").is_err());
    assert!(parse_changes("R100\0\0new\0").is_err());
    assert!(parse_changes("R100\0old\0\0").is_err());
}

#[test]
fn parse_changes_refuses_an_empty_or_unknown_status() {
    assert!(parse_changes("\0a\0").is_err());
    assert!(parse_changes("1\0a\0").is_err());
    assert!(parse_changes("R\0a\0b\0").is_err());
    assert!(parse_changes("A1\0a\0").is_err());
    // The old line format is no longer read.
    assert!(parse_changes("M\tREADME.md\n").is_err());
}

#[test]
fn parse_changes_keeps_odd_paths_as_written() {
    assert_eq!(
        parse_changes("A\0a\tb\0M\0c\nd\0A\0\"q\"\0A\0\u{e9}.json\0").unwrap(),
        vec![
            Change::Added("a\tb".into()),
            Change::Modified("c\nd".into()),
            Change::Added("\"q\"".into()),
            Change::Added("\u{e9}.json".into()),
        ]
    );
}

// ---- check_proposal --------------------------------------------------------------------------

#[test]
fn a_change_outside_research_passes_untouched() {
    let (base, head) = trees(&[]);
    let changes = vec![modified("crates/x.rs")];
    assert!(run_person(&changes, &base, &head).is_empty());
}

#[test]
fn adding_the_first_config_of_a_new_family_passes() {
    let (base, head) = trees(&[]);
    head.put(&config("trend-v1", "trend", today(), Value::Null));
    let changes = added_config("trend-v1");
    assert_eq!(run(&changes, &base, &head), Vec::<String>::new());
}

#[test]
fn outside_paths_in_a_research_pr_fail_and_are_named() {
    let (base, head) = trees(&[]);
    head.put(&config("trend-v1", "trend", today(), Value::Null));
    for outside in [
        "crates/risk/src/gate.rs",
        ".github/workflows/ci.yml",
        "config/limits/limits.json",
        "research/signed/x.json",
        "research/agents/overnight.md",
        "research/README.md",
    ] {
        let mut changes = added_config("trend-v1");
        changes.push(modified(outside));
        let failures = run(&changes, &base, &head);
        assert!(
            failures.iter().any(|f| f.contains(outside)),
            "{outside}: {failures:?}"
        );
    }
}

#[test]
fn deleting_a_note_fails() {
    let (base, head) = trees(&[config("trend-v1", "trend", day(0), Value::Null)]);
    let changes = vec![Change::Deleted("research/notes/trend-v1.md".into())];
    let failures = run(&changes, &base, &head);
    assert!(
        failures.iter().any(|f| f.contains("is deleted")),
        "{failures:?}"
    );
    // A person may reorganise research files; only an agent may not.
    assert_eq!(run_person(&changes, &base, &head), Vec::<String>::new());
}

#[test]
fn renaming_a_note_fails() {
    let (base, head) = trees(&[config("trend-v1", "trend", day(0), Value::Null)]);
    let changes = vec![Change::Renamed {
        from: "research/notes/trend-v1.md".into(),
        to: "research/notes/trend-v2.md".into(),
    }];
    assert!(!run(&changes, &base, &head).is_empty());
}

#[test]
fn two_configs_added_fail() {
    let (base, head) = trees(&[]);
    head.put(&config("trend-a", "trend", day(0), Value::Null));
    head.put(&config("trend-b", "trend", day(0), Value::Null));
    let mut changes = added_config("trend-a");
    changes.extend(added_config("trend-b"));
    assert!(!run(&changes, &base, &head).is_empty());
}

#[test]
fn a_config_in_a_subdirectory_or_not_json_fails() {
    let (base, head) = trees(&[]);
    head.put(&config("trend-v1", "trend", today(), Value::Null));
    let changes = vec![added("research/configs/sub/trend-v1.json")];
    assert!(!run(&changes, &base, &head).is_empty());
    let changes = vec![added("research/configs/trend-v1.txt")];
    assert!(!run(&changes, &base, &head).is_empty());
}

#[test]
fn a_config_without_its_note_fails() {
    let (base, head) = trees(&[]);
    head.write(
        "research/configs/trend-v1.json",
        &config("trend-v1", "trend", today(), Value::Null).to_string(),
    );
    let changes = vec![added("research/configs/trend-v1.json")];
    let failures = run(&changes, &base, &head);
    assert!(failures.iter().any(|f| f.contains("note")), "{failures:?}");
}

#[test]
fn a_note_without_a_heading_fails() {
    let (base, head) = trees(&[]);
    head.put(&config("trend-v1", "trend", today(), Value::Null));
    head.write("research/notes/trend-v1.md", "## Economic rationale\n\nx\n");
    let failures = run(&added_config("trend-v1"), &base, &head);
    assert!(!failures.is_empty());
}

#[test]
fn a_file_stem_that_differs_from_the_name_fails() {
    let (base, head) = trees(&[]);
    let value = config("trend-b", "trend", today(), Value::Null);
    head.write("research/configs/other.json", &value.to_string());
    head.write("research/notes/trend-b.md", VALID_NOTE);
    let changes = vec![added("research/configs/other.json")];
    let failures = run(&changes, &base, &head);
    assert!(
        failures.iter().any(|f| f.contains("other.json")),
        "{failures:?}"
    );
}

#[test]
fn a_config_created_in_the_future_fails() {
    let (base, head) = trees(&[]);
    head.put(&config(
        "trend-v1",
        "trend",
        date(2026, 11, 28),
        Value::Null,
    ));
    assert!(!run(&added_config("trend-v1"), &base, &head).is_empty());
}

#[test]
fn a_second_config_of_a_family_29_days_after_the_first_fails() {
    let first = config("trend-a", "trend", day(0), Value::Null);
    let (base, head) = trees(std::slice::from_ref(&first));
    head.put(&config("trend-b", "trend", day(29), json!("trend-a")));
    assert!(!run(&added_config("trend-b"), &base, &head).is_empty());
}

#[test]
fn a_second_config_of_a_family_30_days_after_the_first_passes() {
    let first = config("trend-a", "trend", day(0), Value::Null);
    let (base, head) = trees(std::slice::from_ref(&first));
    head.put(&config("trend-b", "trend", day(30), json!("trend-a")));
    assert_eq!(
        run(&added_config("trend-b"), &base, &head),
        Vec::<String>::new()
    );
}

#[test]
fn a_parent_missing_at_the_base_fails() {
    let (base, head) = trees(&[]);
    head.put(&config("trend-b", "trend", day(40), json!("trend-a")));
    assert!(!run(&added_config("trend-b"), &base, &head).is_empty());
}

#[test]
fn a_parent_of_another_family_fails() {
    let other = config("rates-a", "rates", day(0), Value::Null);
    let (base, head) = trees(std::slice::from_ref(&other));
    head.put(&config("trend-b", "trend", day(40), json!("rates-a")));
    let failures = run(&added_config("trend-b"), &base, &head);
    assert!(
        failures.iter().any(|f| f.contains("family")),
        "{failures:?}"
    );
}

#[test]
fn null_parent_when_the_family_exists_fails() {
    let first = config("trend-a", "trend", day(0), Value::Null);
    let (base, head) = trees(std::slice::from_ref(&first));
    head.put(&config("trend-b", "trend", day(40), Value::Null));
    let failures = run(&added_config("trend-b"), &base, &head);
    assert!(
        failures.iter().any(|f| f.contains("parent")),
        "{failures:?}"
    );
}

#[test]
fn modifying_a_config_in_place_after_30_days_passes() {
    let old = config("trend-v1", "trend", day(0), Value::Null);
    let (base, head) = trees(std::slice::from_ref(&old));
    // Head: same name and family, a newer date, a different parameter.
    let mut new = config("trend-v1", "trend", day(31), Value::Null);
    new["strategy"]["lookback_days"] = json!(126);
    head.put(&new);
    let changes = vec![modified("research/configs/trend-v1.json")];
    assert_eq!(run(&changes, &base, &head), Vec::<String>::new());
}

#[test]
fn modifying_a_config_in_place_too_soon_fails() {
    let old = config("trend-v1", "trend", day(0), Value::Null);
    let (base, head) = trees(std::slice::from_ref(&old));
    head.put(&config("trend-v1", "trend", day(10), Value::Null));
    let changes = vec![modified("research/configs/trend-v1.json")];
    assert!(!run(&changes, &base, &head).is_empty());
}

#[test]
fn modifying_a_config_into_another_family_fails() {
    let old = config("trend-v1", "trend", day(0), Value::Null);
    let (base, head) = trees(std::slice::from_ref(&old));
    head.put(&config("trend-v1", "rates", day(40), Value::Null));
    let changes = vec![modified("research/configs/trend-v1.json")];
    let failures = run(&changes, &base, &head);
    assert!(
        failures.iter().any(|f| f.contains("family")),
        "{failures:?}"
    );
}

#[test]
fn a_path_with_dot_dot_fails_and_reads_nothing_outside() {
    let (base, head) = trees(&[]);
    let changes = vec![added("research/configs/../../x")];
    let failures = run(&changes, &base, &head);
    assert!(!failures.is_empty());
    assert!(
        failures
            .iter()
            .any(|f| f.contains("research/configs/../../x"))
    );
}

#[test]
fn several_violations_are_all_reported() {
    let (base, head) = trees(&[]);
    head.put(&config("trend-a", "trend", date(2026, 12, 1), Value::Null));
    head.put(&config("trend-b", "trend", date(2026, 12, 1), Value::Null));
    let mut changes = added_config("trend-a");
    changes.extend(added_config("trend-b"));
    changes.push(modified("crates/risk/src/gate.rs"));
    let failures = run(&changes, &base, &head);
    assert!(failures.len() >= 2, "{failures:?}");
}

// ---- is_plain_path and rule 1 ----------------------------------------------------------------

#[test]
fn is_plain_path_accepts_only_the_plain_form() {
    for good in ["research/notes/a.md", "a..b", "a-b_c.9", "x/y"] {
        assert!(is_plain_path(good), "{good:?}");
    }
    for bad in [
        "", "/a", "a/", "./a", "a/./b", "a/../b", "a//b", "a\\b", "a b", "a\"b", "\u{e9}", "a\tb",
        "a\nb", "a:b",
    ] {
        assert!(!is_plain_path(bad), "{bad:?}");
    }
}

#[test]
fn a_path_that_is_not_plain_fails_under_rule_1_in_both_modes() {
    let (base, head) = trees(&[]);
    head.put(&config("trend-v1", "trend", today(), Value::Null));
    for bad in [
        "research/notes/a\"b.md",
        "research/notes/a b.md",
        "research/notes/\u{e9}.md",
        "research/notes/a\\b.md",
        "research/configs/./x.json",
        "research//x",
        "research/configs/../x.json",
        "research/configs/x/",
    ] {
        for agent in [false, true] {
            let changes = vec![added(bad)];
            let failures = check_proposal(&changes, agent, base.path(), head.path(), today());
            let shown = format!("{bad:?}");
            assert!(
                failures
                    .iter()
                    .any(|f| f.contains("is not a plain path") && f.contains(&shown)),
                "{bad:?} agent={agent}: {failures:?}"
            );
        }
    }
    // A leading slash is not under research/, so only an agent pull request reaches rule 1 for it.
    let changes = vec![added("/research/configs/x.json")];
    assert_eq!(run_person(&changes, &base, &head), Vec::<String>::new());
    let failures = run(&changes, &base, &head);
    assert!(
        failures.iter().any(|f| f.contains("is not a plain path")),
        "{failures:?}"
    );
}

#[test]
fn a_non_plain_research_path_is_never_read() {
    // The config under the odd name is valid JSON, but its name is not plain, so nothing reads it.
    let (base, head) = trees(&[]);
    head.put(&config("trend-v1", "trend", today(), Value::Null));
    let odd = config("trend-v2", "trend", today(), Value::Null).to_string();
    head.write("research/configs/a b.json", &odd);
    let changes = vec![added("research/configs/a b.json")];
    let failures = run(&changes, &base, &head);
    assert!(
        failures.iter().any(|f| f.contains("is not a plain path")),
        "{failures:?}"
    );
    assert!(
        !failures.iter().any(|f| f.contains("trend-v2")),
        "{failures:?}"
    );
}

#[test]
fn read_regular_reads_a_plain_regular_file_and_refuses_the_rest() {
    let tree = Tree::new();
    tree.write("research/notes/a.md", "text");
    assert_eq!(
        read_regular(tree.path(), "research/notes/a.md").unwrap(),
        "text"
    );
    let missing = read_regular(tree.path(), "research/notes/missing.md").unwrap_err();
    assert!(missing.contains("missing.md"), "{missing}");
    let dotdot = read_regular(tree.path(), "research/../notes/a.md").unwrap_err();
    assert!(dotdot.contains("is not a plain path"), "{dotdot}");
    let dir = read_regular(tree.path(), "research/notes").unwrap_err();
    assert!(dir.contains("is not a regular file"), "{dir}");
}

#[cfg(unix)]
#[test]
fn read_regular_does_not_follow_a_link_on_the_way() {
    let tree = Tree::new();
    tree.write("research/notes/a.md", "text");
    tree.link("linked", "research");
    let err = read_regular(tree.path(), "linked/notes/a.md").unwrap_err();
    assert!(err.contains("linked/notes/a.md"), "{err}");
}

// ---- agent mode ------------------------------------------------------------------------------

#[test]
fn an_agent_pull_request_must_change_research_files() {
    let (base, head) = trees(&[]);
    head.put(&config("trend-v1", "trend", today(), Value::Null));

    // Only limits changed: a person's pull request passes, an agent's fails.
    let limits = vec![modified("config/limits/limits.json")];
    assert_eq!(run_person(&limits, &base, &head), Vec::<String>::new());
    let failures = run(&limits, &base, &head);
    assert!(
        failures
            .iter()
            .any(|f| f.contains("config/limits/limits.json")),
        "{failures:?}"
    );

    // Only a workflow changed: fails for an agent, rule 1 names it.
    let workflow = vec![modified(".github/workflows/sign.yml")];
    let failures = run(&workflow, &base, &head);
    assert!(
        failures
            .iter()
            .any(|f| f.contains(".github/workflows/sign.yml")),
        "{failures:?}"
    );

    // An empty change list fails for an agent only.
    assert_eq!(run_person(&[], &base, &head), Vec::<String>::new());
    let failures = run(&[], &base, &head);
    assert!(
        failures
            .iter()
            .any(|f| f.contains("must change research files")),
        "{failures:?}"
    );
}

#[test]
fn a_valid_first_config_passes_in_both_modes() {
    let (base, head) = trees(&[]);
    head.put(&config("trend-v1", "trend", today(), Value::Null));
    let changes = added_config("trend-v1");
    assert_eq!(run(&changes, &base, &head), Vec::<String>::new());
    assert_eq!(run(&changes, &base, &head), Vec::<String>::new());
}

// ---- the budget by strategy kind -------------------------------------------------------------

#[test]
fn a_new_family_with_the_same_strategy_kind_does_not_reset_the_budget() {
    let first = config("trend-v1", "trend", day(0), Value::Null);
    let (base, head) = trees(std::slice::from_ref(&first));
    head.put(&config("trend-b", "trendb", day(0), Value::Null));
    let failures = run(&added_config("trend-b"), &base, &head);
    assert!(
        failures.iter().any(|f| f.contains("trend-v1")),
        "{failures:?}"
    );
}

#[test]
fn a_new_family_with_the_same_strategy_kind_passes_30_days_later() {
    let first = config("trend-v1", "trend", day(0), Value::Null);
    let (base, head) = trees(std::slice::from_ref(&first));
    head.put(&config("trend-b", "trendb", day(30), Value::Null));
    assert_eq!(
        run(&added_config("trend-b"), &base, &head),
        Vec::<String>::new()
    );
}

// ---- symbolic links --------------------------------------------------------------------------

#[cfg(unix)]
#[test]
fn a_note_that_is_a_symlink_fails() {
    let first = config("trend-v1", "trend", day(0), Value::Null);
    let (base, head) = trees(std::slice::from_ref(&first));
    let cfg = config("trend-b", "trend", day(40), json!("trend-v1"));
    head.write("research/configs/trend-b.json", &cfg.to_string());
    head.write("research/notes/other.md", VALID_NOTE);
    head.link("research/notes/trend-b.md", "other.md");
    let failures = run(&added_config("trend-b"), &base, &head);
    assert!(
        failures
            .iter()
            .any(|f| f.contains("research/notes/trend-b.md is not a regular file")),
        "{failures:?}"
    );
}

#[cfg(unix)]
#[test]
fn a_config_that_is_a_symlink_fails() {
    let (base, head) = trees(&[]);
    head.write(
        "research/elsewhere/trend-v1.json",
        &config("trend-v1", "trend", today(), Value::Null).to_string(),
    );
    head.write("research/notes/trend-v1.md", VALID_NOTE);
    head.link(
        "research/configs/trend-v1.json",
        "../elsewhere/trend-v1.json",
    );
    let failures = run(&added_config("trend-v1"), &base, &head);
    assert!(
        failures
            .iter()
            .any(|f| f.contains("research/configs/trend-v1.json")
                && f.contains("is not a regular file")),
        "{failures:?}"
    );
}

#[cfg(unix)]
#[test]
fn a_symlinked_json_file_outside_the_diff_fails() {
    let (base, head) = trees(&[]);
    head.put(&config("trend-v1", "trend", today(), Value::Null));
    head.link("research/configs/extra.json", "../../outside.json");
    let failures = run(&added_config("trend-v1"), &base, &head);
    assert!(
        failures.iter().any(|f| f.contains("extra.json")),
        "{failures:?}"
    );
}

#[cfg(unix)]
#[test]
fn a_symlinked_json_file_in_the_base_fails_the_budget_check() {
    let (base, head) = trees(&[]);
    base.link("research/configs/old.json", "../../outside.json");
    head.put(&config("trend-v1", "trend", today(), Value::Null));
    let failures = run(&added_config("trend-v1"), &base, &head);
    assert!(
        failures.iter().any(|f| f.contains("old.json")),
        "{failures:?}"
    );
}

// ---- budget_conflicts ------------------------------------------------------------------------

#[test]
fn budget_conflicts_with_no_other_config_is_empty() {
    let tree = Tree::new();
    tree.put(&config("trend-v1", "trend", day(0), Value::Null));
    assert_eq!(
        budget_conflicts(tree.path(), "trend-v1").unwrap(),
        Vec::<String>::new()
    );
}

#[test]
fn budget_conflicts_names_a_same_family_config_10_days_away() {
    let tree = Tree::new();
    tree.put(&config("trend-a", "trend", day(0), Value::Null));
    tree.put(&config("trend-b", "trend", day(10), json!("trend-a")));
    let later = budget_conflicts(tree.path(), "trend-b").unwrap();
    assert_eq!(later.len(), 1, "{later:?}");
    assert!(later[0].contains("trend-a"), "{later:?}");
    // The earlier one was within its budget when it was made: it stays signable.
    let earlier = budget_conflicts(tree.path(), "trend-a").unwrap();
    assert_eq!(earlier, Vec::<String>::new());
}

#[test]
fn budget_conflicts_names_a_same_kind_config_29_days_earlier() {
    let tree = Tree::new();
    tree.put(&config("trend-a", "trend", day(0), Value::Null));
    tree.put(&config("trend-b", "trendb", day(29), Value::Null));
    let messages = budget_conflicts(tree.path(), "trend-b").unwrap();
    assert_eq!(messages.len(), 1, "{messages:?}");
    assert!(messages[0].contains("trend-a"), "{messages:?}");
}

#[test]
fn budget_conflicts_is_empty_30_days_apart() {
    let tree = Tree::new();
    tree.put(&config("trend-a", "trend", day(0), Value::Null));
    tree.put(&config("trend-b", "trendb", day(30), Value::Null));
    assert_eq!(
        budget_conflicts(tree.path(), "trend-b").unwrap(),
        Vec::<String>::new()
    );
    // The same family 30 days apart is clear too.
    // Family trend, 30 days after trend-b (day 30) and 60 after trend-a (day 0).
    tree.put(&config("trend-c", "trend", day(60), json!("trend-a")));
    assert_eq!(
        budget_conflicts(tree.path(), "trend-c").unwrap(),
        Vec::<String>::new()
    );
}

#[test]
fn budget_conflicts_for_an_unknown_name_is_an_error() {
    let tree = Tree::new();
    tree.put(&config("trend-a", "trend", day(0), Value::Null));
    let err = budget_conflicts(tree.path(), "no-such").unwrap_err();
    assert!(matches!(err, EvalError::InvalidProposal(_)), "{err:?}");
}

#[test]
fn budget_conflicts_refuses_a_config_that_does_not_parse() {
    let tree = Tree::new();
    tree.put(&config("trend-a", "trend", day(0), Value::Null));
    tree.write("research/configs/bad.json", "{");
    let err = budget_conflicts(tree.path(), "trend-a").unwrap_err();
    assert!(err.to_string().contains("bad.json"), "{err}");
}

#[cfg(unix)]
#[test]
fn budget_conflicts_refuses_a_symlinked_config() {
    let tree = Tree::new();
    tree.put(&config("trend-a", "trend", day(0), Value::Null));
    tree.link("research/configs/link.json", "trend-a.json");
    assert!(budget_conflicts(tree.path(), "trend-a").is_err());
}

#[test]
fn a_persons_pull_request_may_change_code_beside_a_config_but_keeps_the_budget() {
    // Code and a first config together: fine for a person, refused for an agent.
    let (base, head) = trees(&[]);
    head.put(&config("trend-v1", "trend", today(), Value::Null));
    let mut changes = added_config("trend-v1");
    changes.push(modified("crates/eval/src/proposal.rs"));
    changes.push(modified("research/agents/overnight.md"));
    assert_eq!(run_person(&changes, &base, &head), Vec::<String>::new());
    assert!(!run(&changes, &base, &head).is_empty());

    // The budget and the one-config rule bind a person too.
    let (base, head) = trees(&[config("trend-v1", "trend", day(0), Value::Null)]);
    head.put(&config("trend-v2", "trend", day(5), json!("trend-v1")));
    let failures = run_person(&added_config("trend-v2"), &base, &head);
    assert!(
        failures.iter().any(|f| f.contains("less than 30 days")),
        "{failures:?}"
    );
}

#[test]
fn a_back_dated_proposal_fails() {
    // The newest config is long ago; a proposal dated a month after it, but not today, would
    // let several be merged in one day.
    let first = config("trend-a", "trend", date(2026, 6, 1), Value::Null);
    let (base, head) = trees(std::slice::from_ref(&first));
    head.put(&config(
        "trend-b",
        "trend",
        date(2026, 7, 1),
        json!("trend-a"),
    ));
    let failures = run(&added_config("trend-b"), &base, &head);
    assert!(
        failures.iter().any(|f| f.contains("days before today")),
        "{failures:?}"
    );

    // Dated within the week: fine.
    let (base, head) = trees(std::slice::from_ref(&first));
    head.put(&config(
        "trend-b",
        "trend",
        date(2026, 11, 20),
        json!("trend-a"),
    ));
    assert_eq!(
        run(&added_config("trend-b"), &base, &head),
        Vec::<String>::new()
    );
}

#[test]
fn hidden_files_are_not_plain_paths() {
    for bad in [
        "research/notes/.gitattributes",
        ".github/x",
        "research/.hidden/a.md",
    ] {
        assert!(!is_plain_path(bad), "{bad:?}");
    }
    let (base, head) = trees(&[]);
    let failures = run(&[added("research/notes/.gitattributes")], &base, &head);
    assert!(
        failures.iter().any(|f| f.contains("is not a plain path")),
        "{failures:?}"
    );
}

#[test]
fn a_changed_config_that_is_not_in_the_head_tree_fails() {
    // The diff says a config was added, but the tree handed to the checker does not have it
    // (however that came about): it must not pass unjudged.
    let (base, head) = trees(&[config("trend-v1", "trend", day(0), Value::Null)]);
    for agent in [false, true] {
        let failures = check_proposal(
            &added_config("trend-v3"),
            agent,
            base.path(),
            head.path(),
            today(),
        );
        assert!(
            failures
                .iter()
                .any(|f| f.contains("research/configs/trend-v3.json")),
            "agent={agent}: {failures:?}"
        );
    }
}
