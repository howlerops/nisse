//! Checking, before anything is traded, that the engine's config is a signed deployment. See
//! `docs/phases/phase-7.md`, "Signing and verification".
//!
//! With `signature.required` false the check reads nothing and reports the deployment as
//! unsigned. With it true, the manifest must exist, verify under the public key in
//! [`ENV_VERIFY_KEY`], be for the paper stage and name exactly the engine's deployment hash.
//! Every refusal is an [`AppError::Unsigned`] whose message never contains the key. The engine
//! never holds the secret signing key: it can check a manifest but cannot make one.

use std::path::Path;

use nisse_core::Hash32;
use nisse_core::signing::{Manifest, STAGE_PAPER, VerifyKey};

use crate::AppError;
use crate::config::EngineConfig;

pub use nisse_core::signing::ENV_VERIFY_KEY;

/// What the check found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Deployment {
    /// A manifest for exactly this deployment verified.
    Signed {
        name: String,
        git_sha: String,
        deployment_hash: Hash32,
    },
    /// The config does not require a signature and none was checked.
    Unsigned { deployment_hash: Hash32 },
}

/// Checks the engine's deployment against its manifest. `key_hex` is the value of
/// `ENV_VERIFY_KEY` if set.
pub fn verify_deployment(
    config: &EngineConfig,
    key_hex: Option<&str>,
) -> Result<Deployment, AppError> {
    let deployment_hash = config.deployment_hash()?;
    if !config.signature.required {
        return Ok(Deployment::Unsigned { deployment_hash });
    }

    let key_text = key_hex.map(str::trim).filter(|text| !text.is_empty());
    let Some(key_text) = key_text else {
        return Err(unsigned(format!("{ENV_VERIFY_KEY} is not set")));
    };
    // The parse error never includes the key's text (see `VerifyKey::from_hex`).
    let key = VerifyKey::from_hex(key_text).map_err(|e| unsigned(e.to_string()))?;

    let manifest = read_manifest(&config.signature.manifest)?;
    manifest
        .verify(&key)
        .map_err(|_| unsigned("the manifest's signature does not verify"))?;

    if manifest.stage != STAGE_PAPER {
        return Err(unsigned(format!(
            "the manifest is for stage {:?}, not {STAGE_PAPER:?}",
            manifest.stage
        )));
    }
    if manifest.deployment_hash != deployment_hash {
        return Err(unsigned(format!(
            "the engine's config is not the deployment that was signed: \
             the config hashes to {deployment_hash}, the manifest signs {}",
            manifest.deployment_hash
        )));
    }

    Ok(Deployment::Signed {
        name: manifest.name,
        git_sha: manifest.git_sha,
        deployment_hash,
    })
}

/// [`verify_deployment`] with the key read from [`ENV_VERIFY_KEY`]. Every command that can
/// trade calls this first, inside the command itself, so that no caller can forget it.
pub fn verify_from_env(config: &EngineConfig) -> Result<Deployment, AppError> {
    let key = std::env::var(ENV_VERIFY_KEY).ok();
    verify_deployment(config, key.as_deref())
}

/// An `Unsigned` refusal with the given message.
fn unsigned(message: impl Into<String>) -> AppError {
    AppError::Unsigned(message.into())
}

/// Reads and parses the manifest file. Unreadable, not JSON or not a manifest is `Unsigned`
/// naming the path. Does not verify.
fn read_manifest(path: &Path) -> Result<Manifest, AppError> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| unsigned(format!("cannot read manifest {}: {e}", path.display())))?;
    let value: serde_json::Value = serde_json::from_str(&text)
        .map_err(|e| unsigned(format!("manifest {} is not JSON: {e}", path.display())))?;
    Manifest::from_json(&value).map_err(|e| {
        unsigned(format!(
            "manifest {} is not a manifest: {e}",
            path.display()
        ))
    })
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use std::path::PathBuf;
    use std::process;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use serde_json::json;

    use nisse_core::signing::SigningKey;

    use super::*;
    use crate::config::SignatureRule;

    const GIT_SHA: &str = "0123456789abcdef0123456789abcdef01234567";
    const SIGNED_AT: &str = "2026-11-27T00:00:00Z";
    const KEY_TEXT: &str = "3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c";
    const OTHER_KEY_TEXT: &str = "d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4";
    const MANIFEST_NAME: &str = "trend-v1.manifest.json";

    /// A directory under the system temp dir, unique to this process and call, removed on drop.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            static COUNTER: AtomicUsize = AtomicUsize::new(0);
            let n = COUNTER.fetch_add(1, Ordering::SeqCst);
            let path =
                std::env::temp_dir().join(format!("nisse-deployment-test-{}-{n}", process::id()));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn manifest_path(&self) -> PathBuf {
            self.0.join(MANIFEST_NAME)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn key() -> SigningKey {
        SigningKey::from_hex(KEY_TEXT).unwrap()
    }

    /// The public key matching `key()`, as the engine host would hold it.
    fn verify_text() -> String {
        key().verify_key().to_string()
    }

    /// The repository's paper config, with the signature rule pointing into `dir`.
    fn config_in(dir: &TempDir, required: bool) -> EngineConfig {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../config/engine/paper.json"
        );
        let mut config = EngineConfig::load(Path::new(path)).unwrap();
        config.signature = SignatureRule {
            manifest: dir.manifest_path(),
            required,
        };
        config
    }

    /// Signs a manifest for `config`'s deployment with `signing_key` and writes it to the
    /// temp dir.
    fn write_signed(dir: &TempDir, config: &EngineConfig, signing_key: &SigningKey) -> Manifest {
        let manifest = Manifest::sign(
            signing_key,
            "trend-v1",
            config.deployment_hash().unwrap(),
            GIT_SHA,
            STAGE_PAPER,
            SIGNED_AT,
        )
        .unwrap();
        write_json(dir, &manifest.to_json());
        manifest
    }

    fn write_json(dir: &TempDir, value: &serde_json::Value) {
        std::fs::write(dir.manifest_path(), value.to_string()).unwrap();
    }

    /// The error of a refused check. Panics on acceptance or on any other error kind.
    fn refused(config: &EngineConfig, key_hex: Option<&str>) -> String {
        match verify_deployment(config, key_hex) {
            Ok(outcome) => panic!("expected a refusal, got {outcome:?}"),
            Err(AppError::Unsigned(message)) => message,
            Err(other) => panic!("expected Unsigned, got {other:?}"),
        }
    }

    #[test]
    fn not_required_is_unsigned_without_manifest_or_key() {
        let dir = TempDir::new();
        let config = config_in(&dir, false);
        assert!(!dir.manifest_path().exists());
        let outcome = verify_deployment(&config, None).unwrap();
        assert_eq!(
            outcome,
            Deployment::Unsigned {
                deployment_hash: config.deployment_hash().unwrap()
            }
        );
    }

    #[test]
    fn not_required_ignores_a_broken_manifest_and_key() {
        let dir = TempDir::new();
        write_json(&dir, &json!("not a manifest"));
        let config = config_in(&dir, false);
        let outcome = verify_deployment(&config, Some("zz")).unwrap();
        assert!(matches!(outcome, Deployment::Unsigned { .. }));
    }

    #[test]
    fn required_and_signed_for_this_deployment_is_signed() {
        let dir = TempDir::new();
        let config = config_in(&dir, true);
        write_signed(&dir, &config, &key());
        let outcome = verify_deployment(&config, Some(&verify_text())).unwrap();
        assert_eq!(
            outcome,
            Deployment::Signed {
                name: "trend-v1".to_owned(),
                git_sha: GIT_SHA.to_owned(),
                deployment_hash: config.deployment_hash().unwrap(),
            }
        );
    }

    #[test]
    fn surrounding_whitespace_in_the_key_is_accepted() {
        let dir = TempDir::new();
        let config = config_in(&dir, true);
        write_signed(&dir, &config, &key());
        let padded = format!("  {}\n", verify_text());
        assert!(matches!(
            verify_deployment(&config, Some(&padded)),
            Ok(Deployment::Signed { .. })
        ));
    }

    #[test]
    fn required_with_no_key_is_refused() {
        let dir = TempDir::new();
        let config = config_in(&dir, true);
        write_signed(&dir, &config, &key());
        let message = refused(&config, None);
        assert!(message.contains("NISSE_VERIFY_KEY is not set"), "{message}");
    }

    #[test]
    fn required_with_a_blank_key_is_refused() {
        let dir = TempDir::new();
        let config = config_in(&dir, true);
        write_signed(&dir, &config, &key());
        for blank in ["", "   "] {
            let message = refused(&config, Some(blank));
            assert!(message.contains("NISSE_VERIFY_KEY is not set"), "{message}");
        }
    }

    #[test]
    fn required_with_a_bad_key_is_refused_without_echoing_it() {
        let dir = TempDir::new();
        let config = config_in(&dir, true);
        write_signed(&dir, &config, &key());
        let bad = "3c".repeat(10);
        let message = refused(&config, Some(&bad));
        assert!(!message.contains(&bad), "key text leaked: {message}");
        assert!(!message.contains("3c3c"), "key text leaked: {message}");
    }

    #[test]
    fn required_with_a_missing_manifest_is_refused() {
        let dir = TempDir::new();
        let config = config_in(&dir, true);
        let message = refused(&config, Some(&verify_text()));
        assert!(message.contains("cannot read manifest"), "{message}");
        assert!(message.contains(MANIFEST_NAME), "{message}");
    }

    #[test]
    fn required_with_a_manifest_that_is_not_json_is_refused() {
        let dir = TempDir::new();
        let config = config_in(&dir, true);
        std::fs::write(dir.manifest_path(), "{ not json").unwrap();
        let message = refused(&config, Some(&verify_text()));
        assert!(message.contains("is not JSON"), "{message}");
        assert!(message.contains(MANIFEST_NAME), "{message}");
    }

    #[test]
    fn required_with_a_json_value_that_is_not_a_manifest_is_refused() {
        let dir = TempDir::new();
        let config = config_in(&dir, true);
        write_json(&dir, &json!({"name": "trend-v1"}));
        let message = refused(&config, Some(&verify_text()));
        assert!(message.contains("is not a manifest"), "{message}");
    }

    #[test]
    fn required_with_a_manifest_signed_by_another_key_is_refused() {
        let dir = TempDir::new();
        let config = config_in(&dir, true);
        write_signed(
            &dir,
            &config,
            &SigningKey::from_hex(OTHER_KEY_TEXT).unwrap(),
        );
        let message = refused(&config, Some(&verify_text()));
        assert_eq!(message, "the manifest's signature does not verify");
    }

    #[test]
    fn required_with_a_manifest_for_another_deployment_is_refused() {
        let dir = TempDir::new();
        let config = config_in(&dir, true);
        let mut changed = config.clone();
        changed.symbols.pop();
        assert_ne!(
            changed.deployment_hash().unwrap(),
            config.deployment_hash().unwrap()
        );
        write_signed(&dir, &changed, &key());
        let message = refused(&config, Some(&verify_text()));
        assert!(
            message.contains("not the deployment that was signed"),
            "{message}"
        );
        assert!(
            message.contains(&config.deployment_hash().unwrap().to_string()),
            "{message}"
        );
        assert!(
            message.contains(&changed.deployment_hash().unwrap().to_string()),
            "{message}"
        );
    }

    #[test]
    fn changing_a_strategy_parameter_after_signing_is_refused() {
        let dir = TempDir::new();
        let signed_config = config_in(&dir, true);
        write_signed(&dir, &signed_config, &key());
        let mut edited = signed_config;
        edited.strategy["lookback_days"] = json!(253);
        let message = refused(&edited, Some(&verify_text()));
        assert!(
            message.contains("not the deployment that was signed"),
            "{message}"
        );
    }

    #[test]
    fn editing_the_manifest_after_signing_breaks_the_signature() {
        let dir = TempDir::new();
        let config = config_in(&dir, true);
        let manifest = write_signed(&dir, &config, &key());
        let mut json = manifest.to_json();
        json["name"] = json!("trend-v2");
        write_json(&dir, &json);
        let message = refused(&config, Some(&verify_text()));
        assert_eq!(message, "the manifest's signature does not verify");
    }

    #[test]
    fn a_stage_edit_is_caught_by_the_signature_check_first() {
        // `Manifest::sign` refuses any stage but paper, so no manifest with a verifying signature and
        // another stage can be built here; the stage check after `verify` is reached only that
        // way. This checks that an edited stage fails the signature, which runs first.
        let dir = TempDir::new();
        let config = config_in(&dir, true);
        let manifest = write_signed(&dir, &config, &key());
        let mut json = manifest.to_json();
        json["stage"] = json!("live");
        write_json(&dir, &json);
        let message = refused(&config, Some(&verify_text()));
        assert_eq!(message, "the manifest's signature does not verify");
    }

    #[test]
    fn a_key_of_the_wrong_length_is_refused() {
        let dir = TempDir::new();
        let config = config_in(&dir, true);
        write_signed(&dir, &config, &key());
        let short = "3c".repeat(16);
        let message = refused(&config, Some(&short));
        assert!(message.contains("exactly 32 bytes"), "{message}");
    }

    #[test]
    fn the_secret_key_in_place_of_the_public_key_is_refused() {
        // The two are the same length; putting the wrong one on the host must not verify.
        let dir = TempDir::new();
        let config = config_in(&dir, true);
        write_signed(&dir, &config, &key());
        refused(&config, Some(KEY_TEXT));
    }
}
