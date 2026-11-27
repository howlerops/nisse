//! Research configs and the checks a proposal pull request must pass. See
//! `docs/phases/phase-7.md`, "Research config" and "Proposal check".
//!
//! A config is parsed through a private raw struct and then checked field by field; every
//! failure is an [`EvalError::InvalidProposal`] whose message names the field. The proposal
//! check runs on the list of changed paths and on two checkouts (base and head), and collects
//! every failure rather than stopping at the first. It reads only inside the two checkout
//! directories: a changed path is never joined onto them when it contains `..` or starts with
//! `/`.

use std::fs;
use std::path::{Path, PathBuf};

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
/// Days that must pass between two configs of one family.
pub const CHANGE_BUDGET_DAYS: i64 = 30;
/// The headings every note must have, in this order, each with text under it.
pub const NOTE_HEADINGS: [&str; 3] = [
    "## Economic rationale",
    "## Expected effect",
    "## What would change my mind",
];

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
/// The extension every config file has, with its dot.
const JSON_SUFFIX: &str = ".json";
/// The same extension without its dot, as `Path::extension` reports it.
const JSON_EXT: &str = "json";

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

/// How one path changed, as `git diff --name-status` reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    Added(String),
    Modified(String),
    Deleted(String),
    Renamed { from: String, to: String },
    Other { status: String, path: String },
}

/// Parses `git diff --name-status` output. A line is `STATUS<TAB>PATH`, or
/// `RSCORE<TAB>FROM<TAB>TO` for a rename. Blank lines are skipped. A line without a tab, an
/// empty path, or the wrong number of paths for its status is an error.
pub fn parse_changes(text: &str) -> Result<Vec<Change>, EvalError> {
    let mut changes = Vec::new();
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        if !line.contains('\t') {
            return Err(invalid(format!("diff line has no tab: {line:?}")));
        }
        let mut fields = line.split('\t');
        let status = fields.next().unwrap_or_default();
        let paths: Vec<&str> = fields.collect();
        if paths.iter().any(|p| p.is_empty()) {
            return Err(invalid(format!("diff line has an empty path: {line:?}")));
        }
        let one_path = |paths: &[&str]| match paths {
            [path] => Ok((*path).to_owned()),
            _ => Err(invalid(format!("diff line needs one path: {line:?}"))),
        };
        let change = match status {
            "A" => Change::Added(one_path(&paths)?),
            "M" => Change::Modified(one_path(&paths)?),
            "D" => Change::Deleted(one_path(&paths)?),
            _ if status.starts_with('R')
                && status.len() > 1
                && status[1..].bytes().all(|b| b.is_ascii_digit()) =>
            {
                match paths.as_slice() {
                    [from, to] => Change::Renamed {
                        from: (*from).to_owned(),
                        to: (*to).to_owned(),
                    },
                    _ => return Err(invalid(format!("rename line needs two paths: {line:?}"))),
                }
            }
            _ => Change::Other {
                status: status.to_owned(),
                path: one_path(&paths)?,
            },
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

/// The paths a change names: one, or two for a rename.
fn paths_of(change: &Change) -> Vec<&str> {
    match change {
        Change::Added(path)
        | Change::Modified(path)
        | Change::Deleted(path)
        | Change::Other { path, .. } => vec![path.as_str()],
        Change::Renamed { from, to } => vec![from.as_str(), to.as_str()],
    }
}

/// Whether `path` is strictly below directory `dir` (`dir` itself is not).
fn under(dir: &str, path: &str) -> bool {
    path.strip_prefix(dir)
        .is_some_and(|rest| rest.starts_with('/'))
}

/// A path that must never be joined onto a checkout directory.
fn is_unsafe(path: &str) -> bool {
    path.starts_with('/') || path.contains("..")
}

/// The `*.json` files directly in `dir`, sorted. A missing directory is empty.
fn json_files(dir: &Path) -> Result<Vec<PathBuf>, std::io::Error> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut files = Vec::new();
    for entry in entries {
        let path = entry?.path();
        if path.is_file() && path.extension().is_some_and(|ext| ext == JSON_EXT) {
            files.push(path);
        }
    }
    files.sort();
    Ok(files)
}

/// Reads and parses one config. The error text names `shown`, the path as the diff spells it.
fn load_config(path: &Path, shown: &str) -> Result<ResearchConfig, String> {
    let text = fs::read_to_string(path).map_err(|e| format!("{shown}: cannot read it: {e}"))?;
    ResearchConfig::parse(&text).map_err(|e| format!("{shown}: {e}"))
}

/// The name of a file directly in [`CONFIG_DIR`], or `None` for any other path.
fn config_entry(path: &str) -> Option<&str> {
    path.strip_prefix(CONFIG_DIR)?.strip_prefix('/')
}

/// Checks a pull request. `base` and `head` are the two checkouts' root directories; `today`
/// is the current date. Returns every failure, one message each (empty means pass).
pub fn check_proposal(
    changes: &[Change],
    base: &Path,
    head: &Path,
    today: NaiveDate,
) -> Vec<String> {
    if !touches_research(changes) {
        return Vec::new();
    }
    let mut failures = Vec::new();

    // Rules 1 and 2: where the changes are, and that nothing is removed or moved.
    for change in changes {
        for path in paths_of(change) {
            if is_unsafe(path) {
                failures.push(format!(
                    "changed path {path:?} is not a plain relative path under research/configs, research/notes or research/labels"
                ));
            } else if ![CONFIG_DIR, NOTES_DIR, LABELS_DIR]
                .iter()
                .any(|dir| under(dir, path))
            {
                failures.push(format!(
                    "changed path {path:?} is outside research/configs, research/notes and research/labels"
                ));
            }
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
            if is_unsafe(path) || !under(CONFIG_DIR, path) {
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
    let head_configs = head.join(CONFIG_DIR);
    let config_changed = !config_changes.is_empty();
    if !head_configs.is_dir() {
        if config_changed {
            failures.push(format!("no {CONFIG_DIR} directory in the head checkout"));
        }
    } else {
        match json_files(&head_configs) {
            Err(e) => failures.push(format!("cannot read {CONFIG_DIR}: {e}")),
            Ok(files) => {
                for file in files {
                    let shown = format!("{CONFIG_DIR}/{}", file_name(&file));
                    match load_config(&file, &shown) {
                        Err(message) => failures.push(message),
                        Ok(config) => {
                            if Some(config.name.as_str()) != file_stem(&file) {
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
    }

    // Rule 5: the one added or modified config, against the base.
    if let [(is_added, path)] = config_changes.as_slice()
        && !is_unsafe(path)
    {
        check_budget_and_parent(*is_added, path, base, head, today, &mut failures);
    }

    failures
}

/// Rule 4's note check for one config in head.
fn check_note_file(head: &Path, config: &ResearchConfig, failures: &mut Vec<String>) {
    // `parse` has already fixed the rationale to research/notes/<name>.md, so it is safe to join.
    let note = head.join(&config.rationale);
    match fs::read_to_string(&note) {
        Err(_) => failures.push(format!(
            "{}: the note {:?} is missing",
            config.name, config.rationale
        )),
        Ok(text) => {
            if let Err(e) = check_note(&text) {
                failures.push(format!("{}: {e}", config.rationale));
            }
        }
    }
}

/// Rule 5 for the one added or modified config at `path` (a diff path, already known safe).
fn check_budget_and_parent(
    is_added: bool,
    path: &str,
    base: &Path,
    head: &Path,
    today: NaiveDate,
    failures: &mut Vec<String>,
) {
    let head_file = head.join(path);
    // A config that does not parse was reported by rule 4; there is nothing more to check.
    let Ok(config) = load_config(&head_file, path) else {
        return;
    };

    if config.created > today {
        failures.push(format!(
            "{path:?}: created {} is in the future (today is {today})",
            config.created
        ));
    }

    // The base's configs. A base config that does not parse is reported here, by name.
    let base_configs = base.join(CONFIG_DIR);
    let base_files = match json_files(&base_configs) {
        Ok(files) => files,
        Err(e) => {
            failures.push(format!("cannot read the base {CONFIG_DIR}: {e}"));
            return;
        }
    };
    let mut base_parsed: Vec<(String, Result<ResearchConfig, String>)> = Vec::new();
    for file in &base_files {
        let stem = file_stem(file).unwrap_or_default().to_owned();
        let shown = format!("base {CONFIG_DIR}/{}", file_name(file));
        let loaded = load_config(file, &shown);
        if let Err(message) = &loaded {
            failures.push(format!("{message} (the base must be clean)"));
        }
        base_parsed.push((stem, loaded));
    }

    // The newest config of the family at the base, its own old version included.
    let newest = base_parsed
        .iter()
        .filter_map(|(_, loaded)| loaded.as_ref().ok())
        .filter(|other| other.family == config.family)
        .map(|other| other.created)
        .max();
    if let Some(newest) = newest
        && (config.created - newest).num_days() < CHANGE_BUDGET_DAYS
    {
        failures.push(format!(
            "{path:?}: created {} is less than {CHANGE_BUDGET_DAYS} days after {newest}, the newest config of family {:?}",
            config.created, config.family
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
    if !is_added {
        let base_file = base.join(path);
        let shown = format!("base {path}");
        if let Ok(old) = load_config(&base_file, &shown) {
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
}

/// The last component of a path.
fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// The file name without `.json`.
fn file_stem(path: &Path) -> Option<&str> {
    path.file_stem().and_then(|stem| stem.to_str())
}
