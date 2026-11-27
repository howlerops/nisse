//! Research configs and the checks a proposal pull request must pass. See
//! `docs/phases/phase-7.md`, "Research config" and "Proposal check".
//!
//! A config is parsed through a private raw struct and then checked field by field; every
//! failure is an [`EvalError::InvalidProposal`] whose message names the field. The proposal
//! check runs on the list of changed paths and on two checkouts (base and head), and collects
//! every failure rather than stopping at the first. It reads only regular files inside the two
//! checkout directories: a changed path is never joined onto them unless it is a plain path
//! ([`is_plain_path`]), and no symbolic link is followed ([`read_regular`]).

use std::fs;
use std::path::Path;

use chrono::NaiveDate;
use serde_json::Value;

use crate::EvalError;

/// Where configs live, relative to a checkout's root.
pub const CONFIG_DIR: &str = "research/configs";
/// Where notes live, relative to a checkout's root.
pub const NOTES_DIR: &str = "research/notes";
/// Where labels live, relative to a checkout's root.
pub const LABELS_DIR: &str = "research/labels";
/// The research directory. Changes under it are the only ones a proposal may make.
pub const RESEARCH_DIR: &str = "research";
/// Days that must pass between two configs of one family, or of one strategy kind.
pub const CHANGE_BUDGET_DAYS: i64 = 30;
/// How old a new or changed config's `created` date may be when it is proposed.
pub const CREATED_MAX_AGE_DAYS: i64 = 7;
/// The headings every note must have, in this order, each with text under it.
pub const NOTE_HEADINGS: [&str; 3] = [
    "## Economic rationale",
    "## Expected effect",
    "## What would change my mind",
];
/// The extension every config file has, with its dot. Signing refuses any other name too.
pub const JSON_SUFFIX: &str = ".json";

/// The longest `name`.
const MAX_NAME_LEN: usize = 48;
/// The longest `family`.
const MAX_FAMILY_LEN: usize = 32;
/// The longest symbol.
const MAX_SYMBOL_LEN: usize = 12;
/// The shortest and longest `expected_effect`, in characters after trimming.
const MIN_EFFECT_LEN: usize = 20;
const MAX_EFFECT_LEN: usize = 600;
/// The length of a `YYYY-MM-DD` date.
const DATE_LEN: usize = 10;

/// One research config, parsed and checked.
#[derive(Debug, Clone, PartialEq)]
pub struct ResearchConfig {
    pub name: String,
    pub family: String,
    pub created: NaiveDate,
    pub parent: Option<String>,
    pub strategy: serde_json::Value,
    /// Sorted, unique.
    pub symbols: Vec<String>,
    pub benchmark: String,
    pub rationale: String,
    pub expected_effect: String,
}

/// The file as written. Required fields are plain types so that a missing one is refused by
/// serde; `parent` is a [`Value`] for the same reason (an `Option` would default to `None`).
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    name: String,
    family: String,
    created: String,
    parent: Value,
    strategy: Value,
    symbols: Vec<String>,
    benchmark: String,
    rationale: String,
    expected_effect: String,
}

fn invalid(message: impl Into<String>) -> EvalError {
    EvalError::InvalidProposal(message.into())
}

/// Checks a slug: lower-case ASCII letters and digits in non-empty groups joined by single
/// `-`, at most `max_len` bytes. `field` names the setting in the error message.
fn check_slug(field: &str, text: &str, max_len: usize) -> Result<(), EvalError> {
    let well_formed = !text.is_empty()
        && text.split('-').all(|group| {
            !group.is_empty()
                && group
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        });
    if !well_formed {
        return Err(invalid(format!(
            "{field}: {text:?} must be lower-case letters and digits in groups joined by single '-'"
        )));
    }
    if text.len() > max_len {
        return Err(invalid(format!(
            "{field}: {text:?} is longer than {max_len} characters"
        )));
    }
    Ok(())
}

/// Checks a symbol: 1 to 12 characters of upper-case ASCII letters, digits and `.`.
fn check_symbol(field: &str, text: &str) -> Result<(), EvalError> {
    let len = text.len();
    if len == 0 || len > MAX_SYMBOL_LEN {
        return Err(invalid(format!(
            "{field}: symbol {text:?} must be 1 to {MAX_SYMBOL_LEN} characters"
        )));
    }
    let allowed = text
        .bytes()
        .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'.');
    if !allowed {
        return Err(invalid(format!(
            "{field}: symbol {text:?} must be upper-case letters, digits or '.' only"
        )));
    }
    Ok(())
}

/// Checks a `YYYY-MM-DD` date: exactly ten characters that parse as a date.
fn check_date(text: &str) -> Result<NaiveDate, EvalError> {
    if text.len() != DATE_LEN {
        return Err(invalid(format!("created: {text:?} must be YYYY-MM-DD")));
    }
    NaiveDate::parse_from_str(text, "%Y-%m-%d")
        .map_err(|_| invalid(format!("created: {text:?} must be YYYY-MM-DD")))
}

impl ResearchConfig {
    /// Parses and validates the JSON text of one config file.
    pub fn parse(text: &str) -> Result<Self, EvalError> {
        let raw: RawConfig =
            serde_json::from_str(text).map_err(|e| invalid(format!("invalid JSON: {e}")))?;

        check_slug("name", &raw.name, MAX_NAME_LEN)?;
        check_slug("family", &raw.family, MAX_FAMILY_LEN)?;
        let created = check_date(&raw.created)?;

        let parent = match &raw.parent {
            Value::Null => None,
            Value::String(parent) => {
                check_slug("parent", parent, MAX_NAME_LEN)?;
                if *parent == raw.name {
                    return Err(invalid("parent: a config cannot be its own parent"));
                }
                Some(parent.clone())
            }
            _ => return Err(invalid("parent: must be null or a config name")),
        };

        if !raw.strategy.is_object() {
            return Err(invalid("strategy: must be a JSON object"));
        }
        nisse_engine::strategies::build(&raw.strategy)
            .map_err(|e| invalid(format!("strategy: {e}")))?;

        if raw.symbols.is_empty() {
            return Err(invalid("symbols: list at least one symbol"));
        }
        let mut symbols = raw.symbols;
        for symbol in &symbols {
            check_symbol("symbols", symbol)?;
        }
        symbols.sort();
        if let Some(dup) = symbols.windows(2).find(|w| w[0] == w[1]) {
            return Err(invalid(format!("symbols: {:?} is listed twice", dup[0])));
        }

        check_symbol("benchmark", &raw.benchmark)?;

        let expected_rationale = format!("{NOTES_DIR}/{}.md", raw.name);
        if raw.rationale != expected_rationale {
            return Err(invalid(format!(
                "rationale: must be {expected_rationale:?}, got {:?}",
                raw.rationale
            )));
        }

        let effect_len = raw.expected_effect.trim().chars().count();
        if !(MIN_EFFECT_LEN..=MAX_EFFECT_LEN).contains(&effect_len) {
            return Err(invalid(format!(
                "expected_effect: must be {MIN_EFFECT_LEN} to {MAX_EFFECT_LEN} characters after trimming, got {effect_len}"
            )));
        }

        Ok(Self {
            name: raw.name,
            family: raw.family,
            created,
            parent,
            strategy: raw.strategy,
            symbols,
            benchmark: raw.benchmark,
            rationale: raw.rationale,
            expected_effect: raw.expected_effect,
        })
    }
}

/// The strategy kind of a config: the `strategy` string of its strategy object. `parse` has
/// already checked that the kind builds, so this is never empty for a parsed config.
fn strategy_kind(config: &ResearchConfig) -> &str {
    config
        .strategy
        .get("strategy")
        .and_then(Value::as_str)
        .unwrap_or_default()
}

/// Checks a note's text: each of [`NOTE_HEADINGS`] appears as a line of its own, in that order,
/// and has at least one non-blank line that is not a heading before the next `## ` heading or
/// the end.
pub fn check_note(text: &str) -> Result<(), EvalError> {
    let lines: Vec<&str> = text.lines().collect();
    let mut at = [0usize; NOTE_HEADINGS.len()];
    for (slot, heading) in at.iter_mut().zip(NOTE_HEADINGS) {
        *slot = lines
            .iter()
            .position(|line| line.trim() == heading)
            .ok_or_else(|| invalid(format!("note: missing heading {heading:?}")))?;
    }
    if !at.windows(2).all(|w| w[0] < w[1]) {
        return Err(invalid(format!(
            "note: headings must appear in the order {:?}",
            NOTE_HEADINGS
        )));
    }
    for (index, heading) in at.iter().zip(NOTE_HEADINGS) {
        let start = index + 1;
        let body_len = lines
            .get(start..)
            .unwrap_or_default()
            .iter()
            .position(|line| line.trim_start().starts_with("## "))
            .unwrap_or(lines.len() - start);
        let has_text = lines
            .get(start..start + body_len)
            .unwrap_or_default()
            .iter()
            .any(|line| {
                let t = line.trim();
                !t.is_empty() && !t.starts_with('#')
            });
        if !has_text {
            return Err(invalid(format!("note: {heading:?} has no text under it")));
        }
    }
    Ok(())
}

/// How one path changed, as `git diff -z --name-status` reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    Added(String),
    Modified(String),
    Deleted(String),
    /// A rename (`R<score>`): `from` goes away, `to` is new.
    Renamed {
        from: String,
        to: String,
    },
    /// A copy (`C<score>`): `from` stays and `to` is new. Both paths are kept, so both are
    /// checked; `to` is the destination.
    Copied {
        from: String,
        to: String,
    },
    /// Any other single-letter status, such as a type change (`T`). `path` is the only path.
    Other {
        status: String,
        path: String,
    },
}

/// The next field of a record, which must be a non-empty path.
fn take_path<'a>(
    fields: &mut impl Iterator<Item = &'a str>,
    status: &str,
) -> Result<&'a str, EvalError> {
    match fields.next() {
        None => Err(invalid(format!("diff record {status:?} is missing a path"))),
        Some("") => Err(invalid(format!("diff record {status:?} has an empty path"))),
        Some(path) => Ok(path),
    }
}

/// Parses the output of `git diff -z --name-status`. The fields are NUL-separated. A record is
/// a status letter and one path, or `R<score>` or `C<score>` and two paths (from, then to). One
/// trailing NUL is allowed, and empty input is no changes. A missing or empty path, an empty
/// status, or a status that is not a letter or `R`/`C` with a score is an error. Paths are kept
/// exactly as git wrote them; [`check_proposal`] decides which of them are acceptable.
pub fn parse_changes(text: &str) -> Result<Vec<Change>, EvalError> {
    if text.is_empty() {
        return Ok(Vec::new());
    }
    let body = text.strip_suffix('\0').unwrap_or(text);
    let mut fields = body.split('\0');
    let mut changes = Vec::new();
    while let Some(status) = fields.next() {
        let change = match status.as_bytes() {
            [] => return Err(invalid("diff record has an empty status")),
            [b'R' | b'C', digits @ ..]
                if !digits.is_empty() && digits.iter().all(u8::is_ascii_digit) =>
            {
                let from = take_path(&mut fields, status)?.to_owned();
                let to = take_path(&mut fields, status)?.to_owned();
                if status.starts_with('R') {
                    Change::Renamed { from, to }
                } else {
                    Change::Copied { from, to }
                }
            }
            [letter] if letter.is_ascii_alphabetic() && !matches!(*letter, b'R' | b'C') => {
                let path = take_path(&mut fields, status)?.to_owned();
                match *letter {
                    b'A' => Change::Added(path),
                    b'M' => Change::Modified(path),
                    b'D' => Change::Deleted(path),
                    _ => Change::Other {
                        status: status.to_owned(),
                        path,
                    },
                }
            }
            _ => return Err(invalid(format!("unknown diff status {status:?}"))),
        };
        changes.push(change);
    }
    Ok(changes)
}

/// Whether a change list touches anything under `research/` (the path itself or below it).
/// A proposal check with no such change passes untouched.
pub fn touches_research(changes: &[Change]) -> bool {
    changes
        .iter()
        .flat_map(paths_of)
        .any(|path| path == RESEARCH_DIR || under(RESEARCH_DIR, path))
}

/// The paths a change names: one, or two for a rename or a copy.
fn paths_of(change: &Change) -> Vec<&str> {
    match change {
        Change::Added(path)
        | Change::Modified(path)
        | Change::Deleted(path)
        | Change::Other { path, .. } => vec![path.as_str()],
        Change::Renamed { from, to } | Change::Copied { from, to } => {
            vec![from.as_str(), to.as_str()]
        }
    }
}

/// Whether a change adds or modifies a file strictly under `research/`.
fn adds_or_modifies_research(change: &Change) -> bool {
    matches!(change, Change::Added(path) | Change::Modified(path) if under(RESEARCH_DIR, path))
}

/// Whether `path` is strictly below directory `dir` (`dir` itself is not).
fn under(dir: &str, path: &str) -> bool {
    path.strip_prefix(dir)
        .is_some_and(|rest| rest.starts_with('/'))
}

/// Whether `path` is a plain relative path: non-empty, made only of `A-Z a-z 0-9 . _ - /`, not
/// starting with `/`, and with no empty, `.` or `..` component. Only such a path may be joined
/// onto a checkout directory.
#[must_use]
pub fn is_plain_path(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && path
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b'/'))
        && path
            .split('/')
            // No component starts with a dot: that rules out `.`, `..` and hidden files such
            // as `.gitattributes`, which could change how a pull request is displayed.
            .all(|component| !component.is_empty() && !component.starts_with('.'))
}

/// The metadata of `relative` under `root`, found with `symlink_metadata` on every component
/// after `root`, so a symbolic link on the way is seen as a link and is never followed. Every
/// component but the last must be a directory.
fn below(root: &Path, relative: &str) -> std::io::Result<fs::Metadata> {
    let mut path = root.to_path_buf();
    let mut meta = fs::metadata(root)?;
    for part in relative.split('/') {
        if !meta.is_dir() {
            return Err(std::io::Error::other(format!(
                "{relative} has a component that is not a directory"
            )));
        }
        path.push(part);
        meta = fs::symlink_metadata(&path)?;
    }
    Ok(meta)
}

/// Reads the regular file `relative` under the checkout `root` and returns its text. A path
/// that is not a plain path (see [`is_plain_path`]) is refused before anything is read, and so
/// is anything that is not a regular file by `symlink_metadata`: a symbolic link, a directory,
/// or a file reached through one. Every error message names the path.
pub fn read_regular(root: &Path, relative: &str) -> Result<String, String> {
    if !is_plain_path(relative) {
        return Err(format!("{relative:?} is not a plain path"));
    }
    let meta = below(root, relative)
        .map_err(|e| format!("{relative} is missing or not a regular file: {e}"))?;
    if !meta.is_file() {
        return Err(format!("{relative} is not a regular file"));
    }
    fs::read_to_string(root.join(relative)).map_err(|e| format!("{relative} cannot be read: {e}"))
}

/// The names of the `*.json` entries directly in `root/dir`, sorted. A missing directory has
/// none. A `.json` entry that is not a regular file (a symbolic link, say) is pushed onto
/// `failures` and left out of the list. A `dir` that is not a directory, or that is reached
/// through a link, is an error.
fn json_files(root: &Path, dir: &str, failures: &mut Vec<String>) -> Result<Vec<String>, String> {
    match below(root, dir) {
        Ok(meta) if meta.is_dir() => {}
        Ok(_) => return Err(format!("{dir} is not a directory")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("cannot read {dir}: {e}")),
    }
    let entries = fs::read_dir(root.join(dir)).map_err(|e| format!("cannot read {dir}: {e}"))?;
    let mut names = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| format!("cannot read {dir}: {e}"))?;
        let file_name = entry.file_name();
        if !file_name
            .as_encoded_bytes()
            .ends_with(JSON_SUFFIX.as_bytes())
        {
            continue;
        }
        let Ok(name) = file_name.into_string() else {
            failures.push(format!("{dir}: a config file name is not UTF-8"));
            continue;
        };
        // `file_type` does not follow a link, so a symbolic link is reported as one.
        if entry.file_type().is_ok_and(|kind| kind.is_file()) {
            names.push(name);
        } else {
            failures.push(format!(
                "{:?} is not a regular file",
                format!("{dir}/{name}")
            ));
        }
    }
    names.sort();
    Ok(names)
}

/// Reads and parses the config at `relative` under `root`. The error text names `relative`.
fn load_config(root: &Path, relative: &str) -> Result<ResearchConfig, String> {
    let text = read_regular(root, relative)?;
    ResearchConfig::parse(&text).map_err(|e| format!("{relative}: {e}"))
}

/// The name of a file directly in [`CONFIG_DIR`], or `None` for any other path.
fn config_entry(path: &str) -> Option<&str> {
    path.strip_prefix(CONFIG_DIR)?.strip_prefix('/')
}

/// Checks a pull request. `base` and `head` are the two checkouts' root directories; `today`
/// is the current date. `agent` is true when a research agent opened the pull request: then
/// there is no pass for a pull request that changes nothing under `research/`, rule 1 applies
/// to every path, and the change list must add or modify a file under `research/`. Returns
/// every failure, one message each (empty means pass).
pub fn check_proposal(
    changes: &[Change],
    agent: bool,
    base: &Path,
    head: &Path,
    today: NaiveDate,
) -> Vec<String> {
    if !agent && !touches_research(changes) {
        return Vec::new();
    }
    let mut failures = Vec::new();
    if agent && !changes.iter().any(adds_or_modifies_research) {
        failures.push("an agent pull request must change research files".to_owned());
    }

    // Rules 1 and 2 bind an agent's pull request: where its changes are, and that nothing is
    // removed, renamed or copied. A person's own pull request (not `agent`) may change code
    // beside research files and may reorganise them; it is still held to rules 3 to 5, and
    // a research path that is not plain is refused for everyone (it is never opened).
    for change in changes {
        for path in paths_of(change) {
            let in_research = path == RESEARCH_DIR || path.starts_with("research/");
            if !is_plain_path(path) {
                if agent || in_research {
                    failures.push(format!(
                        "changed path {path:?} is not a plain path: only A-Z a-z 0-9 . _ - and / are allowed, with no leading '/' and no empty, '.' or '..' component"
                    ));
                }
            } else if agent
                && ![CONFIG_DIR, NOTES_DIR, LABELS_DIR]
                    .iter()
                    .any(|dir| under(dir, path))
            {
                failures.push(format!(
                    "changed path {path:?} is outside research/configs, research/notes and research/labels"
                ));
            }
        }
        if !agent {
            continue;
        }
        let touches = paths_of(change)
            .into_iter()
            .any(|path| path == RESEARCH_DIR || under(RESEARCH_DIR, path));
        if !touches {
            continue;
        }
        match change {
            Change::Deleted(path) => {
                failures.push(format!(
                    "research file {path:?} is deleted; only additions and changes are allowed"
                ));
            }
            Change::Renamed { from, to } => {
                failures.push(format!(
                    "research file renamed from {from:?} to {to:?}; renames are not allowed"
                ));
            }
            Change::Copied { from, to } => {
                failures.push(format!(
                    "research file copied from {from:?} to {to:?}; copies are not allowed"
                ));
            }
            Change::Other { status, path } => {
                failures.push(format!("research file {path:?} has status {status:?}; only additions and changes are allowed"));
            }
            Change::Added(_) | Change::Modified(_) => {}
        }
    }

    // Rule 3: at most one config added or modified, and only flat `.json` files.
    let config_changes: Vec<(bool, &str)> = changes
        .iter()
        .filter_map(|change| match change {
            Change::Added(path) if under(CONFIG_DIR, path) => Some((true, path.as_str())),
            Change::Modified(path) if under(CONFIG_DIR, path) => Some((false, path.as_str())),
            _ => None,
        })
        .collect();
    if config_changes.len() > 1 {
        failures.push(format!(
            "at most one config may be added or modified per proposal; this one changes {}",
            config_changes.len()
        ));
    }
    for change in changes {
        for path in paths_of(change) {
            if !is_plain_path(path) || !under(CONFIG_DIR, path) {
                continue;
            }
            match config_entry(path) {
                Some(rest) if rest.contains('/') => {
                    failures.push(format!(
                        "{path:?}: subdirectories of research/configs are not allowed"
                    ));
                }
                Some(rest) if rest.strip_suffix(JSON_SUFFIX).is_some() => {}
                _ => failures.push(format!(
                    "{path:?}: configs must be .json files directly in research/configs"
                )),
            }
        }
    }

    // Rule 4: every config in head parses, names its file, and has a note with its headings.
    let config_changed = !config_changes.is_empty();
    if config_changed && !head.join(CONFIG_DIR).is_dir() {
        failures.push(format!("no {CONFIG_DIR} directory in the head checkout"));
    }
    match json_files(head, CONFIG_DIR, &mut failures) {
        Err(e) => failures.push(e),
        Ok(names) => {
            for name in names {
                let shown = format!("{CONFIG_DIR}/{name}");
                match load_config(head, &shown) {
                    Err(message) => failures.push(message),
                    Ok(config) => {
                        if Some(config.name.as_str()) != name.strip_suffix(JSON_SUFFIX) {
                            failures.push(format!(
                                "{shown}: name {:?} must equal the file name without .json",
                                config.name
                            ));
                        }
                        check_note_file(head, &config, &mut failures);
                    }
                }
            }
        }
    }

    // Rule 5: the one added or modified config, against the base. A path that is not plain was
    // refused by rule 1 and is never read.
    if let [(is_added, path)] = config_changes.as_slice()
        && is_plain_path(path)
    {
        check_budget_and_parent(*is_added, path, base, head, today, &mut failures);
    }

    failures
}

/// Rule 4's note check for one config in head.
fn check_note_file(head: &Path, config: &ResearchConfig, failures: &mut Vec<String>) {
    // `parse` has already fixed the rationale to research/notes/<name>.md.
    match read_regular(head, &config.rationale) {
        Err(e) => failures.push(format!("{}: {e}", config.name)),
        Ok(text) => {
            if let Err(e) = check_note(&text) {
                failures.push(format!("{}: {e}", config.rationale));
            }
        }
    }
}

/// Rule 5 for the one added or modified config at `path` (a diff path, already known plain).
fn check_budget_and_parent(
    is_added: bool,
    path: &str,
    base: &Path,
    head: &Path,
    today: NaiveDate,
    failures: &mut Vec<String>,
) {
    // A config that does not parse was reported by rule 4; there is nothing more to check.
    let Ok(config) = load_config(head, path) else {
        return;
    };

    if config.created > today {
        failures.push(format!(
            "{path:?}: created {} is in the future (today is {today})",
            config.created
        ));
    }
    // The budget is counted in `created` dates, so a proposal must carry (nearly) today's:
    // otherwise three proposals back-dated a month apart could all be merged in one day.
    if (today - config.created).num_days() > CREATED_MAX_AGE_DAYS {
        failures.push(format!(
            "{path:?}: created {} is more than {CREATED_MAX_AGE_DAYS} days before today ({today}); a proposal is dated when it is made",
            config.created
        ));
    }

    // The base's configs. A base config that does not parse is reported here, by name.
    let base_names = match json_files(base, CONFIG_DIR, failures) {
        Ok(names) => names,
        Err(e) => {
            failures.push(e);
            return;
        }
    };
    let mut base_parsed: Vec<(String, Result<ResearchConfig, String>)> = Vec::new();
    for name in &base_names {
        let stem = name
            .strip_suffix(JSON_SUFFIX)
            .unwrap_or_default()
            .to_owned();
        let loaded = load_config(base, &format!("{CONFIG_DIR}/{name}"));
        if let Err(message) = &loaded {
            failures.push(format!("{message} (the base must be clean)"));
        }
        base_parsed.push((stem, loaded));
    }

    // The newest config at the base that shares the family or the strategy kind, its own old
    // version included. A new family does not reset the budget for the same strategy.
    let kind = strategy_kind(&config);
    let newest = base_parsed
        .iter()
        .filter_map(|(_, loaded)| loaded.as_ref().ok())
        .filter(|other| other.family == config.family || strategy_kind(other) == kind)
        .max_by_key(|other| other.created);
    if let Some(newest) = newest
        && (config.created - newest.created).num_days() < CHANGE_BUDGET_DAYS
    {
        failures.push(format!(
            "{path:?}: created {} is less than {CHANGE_BUDGET_DAYS} days after {} of config {:?}, the newest at the base with family {:?} or strategy kind {kind:?}",
            config.created, newest.created, newest.name, config.family
        ));
    }

    // The parent, or the rule that a later config of a family names one.
    match &config.parent {
        Some(parent) => {
            let found = base_parsed.iter().find(|(stem, _)| stem == parent);
            match found {
                None => failures.push(format!(
                    "{path:?}: parent {parent:?} does not exist at the base"
                )),
                Some((_, Ok(parent_config))) if parent_config.family != config.family => {
                    failures.push(format!(
                        "{path:?}: parent {parent:?} is in family {:?}, not {:?}",
                        parent_config.family, config.family
                    ));
                }
                Some(_) => {}
            }
        }
        None => {
            // A config's own old version is not a sibling: a family's root may be modified.
            let has_siblings = base_parsed.iter().any(|(stem, loaded)| {
                stem != &config.name
                    && loaded
                        .as_ref()
                        .is_ok_and(|other| other.family == config.family)
            });
            if has_siblings {
                failures.push(format!(
                    "{path:?}: family {:?} already has configs at the base; a later config must name its parent",
                    config.family
                ));
            }
        }
    }

    // A modified config keeps its name and family.
    if !is_added && let Ok(old) = load_config(base, path) {
        if old.name != config.name {
            failures.push(format!(
                "{path:?}: a modified config must keep its name ({:?} at the base, {:?} now)",
                old.name, config.name
            ));
        }
        if old.family != config.family {
            failures.push(format!(
                "{path:?}: a modified config must keep its family ({:?} at the base, {:?} now)",
                old.family, config.family
            ));
        }
    }
}

/// The change budget of one config, re-checked against the configs in `root` (a checkout) when
/// it is signed: one message for every other config that shares its family or its strategy kind
/// and whose `created` date is fewer than [`CHANGE_BUDGET_DAYS`] days before its own (or the
/// same day): the later of a pair is the one in the wrong. Empty means the budget is clear. Symbolic links and configs that do not parse are
/// errors, and so is a `name` that no config in `root` has.
pub fn budget_conflicts(root: &Path, name: &str) -> Result<Vec<String>, EvalError> {
    let mut failures = Vec::new();
    let names = json_files(root, CONFIG_DIR, &mut failures).map_err(invalid)?;
    if !failures.is_empty() {
        return Err(invalid(failures.join("; ")));
    }
    let mut configs = Vec::with_capacity(names.len());
    for file in &names {
        configs.push(load_config(root, &format!("{CONFIG_DIR}/{file}")).map_err(invalid)?);
    }
    let target = configs
        .iter()
        .find(|config| config.name == name)
        .ok_or_else(|| invalid(format!("no config named {name:?} in {CONFIG_DIR}")))?;
    let kind = strategy_kind(target);

    let mut messages = Vec::new();
    for other in configs.iter().filter(|config| config.name != name) {
        let same_family = other.family == target.family;
        let same_kind = strategy_kind(other) == kind;
        // Only the later of a pair is in the wrong: the earlier one was within its budget
        // when it was made and must stay signable. Two with the same date both are.
        let after_other = (target.created - other.created).num_days();
        if (same_family || same_kind) && (0..CHANGE_BUDGET_DAYS).contains(&after_other) {
            let shared = match (same_family, same_kind) {
                (true, true) => "family and strategy kind",
                (true, false) => "family",
                _ => "strategy kind",
            };
            messages.push(format!(
                "{name:?} (created {1}) is less than {CHANGE_BUDGET_DAYS} days after {0:?} (created {2}) and shares its {shared}",
                other.name, target.created, other.created
            ));
        }
    }
    Ok(messages)
}
