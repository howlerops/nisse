//! Signing a merged research config for the paper stage. The key is supplied by the caller;
//! this module never reads the environment or the clock, so it can be tested without a
//! process. The `nisse-sign` binary does the I/O. See `docs/phases/phase-7.md`, "Signing
//! (`nisse-sign`) and verification".
//!
//! Every refusal is an [`EvalError::InvalidProposal`] (or a [`EvalError::Core`] from the
//! signing primitives). No message ever contains the key.

use std::fs;
use std::path::Path;

use nisse_core::signing::{Manifest, STAGE_PAPER, SigningKey, deployment_hash};

use crate::EvalError;
use crate::proposal::{CONFIG_DIR, ResearchConfig, check_note};

/// The environment variable holding the signing key as hex. Never printed.
pub const ENV_SIGNING_KEY: &str = "NISSE_SIGNING_KEY";

/// Where manifests live, relative to a checkout's root.
const SIGNED_DIR: &str = "research/signed";
/// The extension every config file has, with its dot.
const JSON_SUFFIX: &str = ".json";

fn invalid(message: impl Into<String>) -> EvalError {
    EvalError::InvalidProposal(message.into())
}

/// The file stem of `config_path`, which must be `research/configs/<file>.json` directly in
/// that directory: no `..`, no leading `/`, no further directory.
fn config_stem(config_path: &str) -> Result<&str, EvalError> {
    let refuse = || {
        invalid(format!(
            "config path {config_path:?} must be {CONFIG_DIR}/<name>.json"
        ))
    };
    if config_path.contains("..") || config_path.starts_with('/') || config_path.contains('\\') {
        return Err(refuse());
    }
    let file = config_path
        .strip_prefix(CONFIG_DIR)
        .and_then(|rest| rest.strip_prefix('/'))
        .ok_or_else(refuse)?;
    let stem = file.strip_suffix(JSON_SUFFIX).ok_or_else(refuse)?;
    if stem.is_empty() || stem.contains('/') {
        return Err(refuse());
    }
    Ok(stem)
}

/// Signs the research config at `<root>/<config_path>` for the paper stage and returns the
/// manifest. Refuses anything that should not be signed: a path outside the config directory,
/// a config that does not parse or whose name differs from its file stem, a missing or
/// malformed note, a build with uncommitted changes, an unknown (all-zero) commit, or a key
/// that is not a valid hex key of at least `MIN_KEY_BYTES`.
pub fn sign_config(
    root: &Path,
    config_path: &str,
    key_hex: &str,
    git_sha: &str,
    clean_build: bool,
    signed_at: &str,
) -> Result<Manifest, EvalError> {
    let stem = config_stem(config_path)?;

    let text = fs::read_to_string(root.join(config_path))
        .map_err(|_| invalid(format!("cannot read config {config_path}")))?;
    let config = ResearchConfig::parse(&text)?;
    if config.name != stem {
        return Err(invalid(format!(
            "name {:?} must equal the file stem {stem:?}",
            config.name
        )));
    }

    // `ResearchConfig::parse` guarantees `rationale` is exactly `research/notes/<name>.md`.
    let note = fs::read_to_string(root.join(&config.rationale))
        .map_err(|_| invalid(format!("cannot read note {}", config.rationale)))?;
    check_note(&note)?;

    if !clean_build {
        return Err(invalid(
            "this build has uncommitted changes; a build that is not clean does not sign",
        ));
    }
    if git_sha.bytes().all(|b| b == b'0') {
        return Err(invalid(
            "the git sha is unknown (all zeros); sign from a build made from a commit",
        ));
    }

    let key = SigningKey::from_hex(key_hex)?;
    let symbols: Vec<&str> = config.symbols.iter().map(String::as_str).collect();
    let hash = deployment_hash(&config.strategy, &symbols, &config.benchmark)?;
    Ok(Manifest::sign(
        &key,
        &config.name,
        hash,
        git_sha,
        STAGE_PAPER,
        signed_at,
    )?)
}

/// `research/signed/<name>.manifest.json`.
#[must_use]
pub fn manifest_path(name: &str) -> String {
    format!("{SIGNED_DIR}/{name}.manifest.json")
}
