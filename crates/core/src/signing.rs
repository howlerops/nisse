//! Signing a deployment: the hash of what the engine will trade (strategy parameters,
//! universe, benchmark) and an Ed25519 signature over a small manifest naming it. See
//! `docs/phases/phase-7.md`, "Signing and verification".
//!
//! The signer holds a secret key ([`ENV_SIGNING_KEY`]); the engine host holds only the
//! matching public key ([`ENV_VERIFY_KEY`]), which can check a manifest but cannot make one.

use std::fmt;

use chrono::DateTime;
use ed25519_dalek::Signer as _;
use serde_json::{Map, Value};

use crate::hash::hex_value;
use crate::{CoreError, Hash32, canonical_json, config_hash};

/// The environment variable holding the secret signing key as hex. Read only by the signer;
/// its value is never printed.
pub const ENV_SIGNING_KEY: &str = "NISSE_SIGNING_KEY";

/// The environment variable holding the public verification key as hex. Read by the engine.
/// Not a secret.
pub const ENV_VERIFY_KEY: &str = "NISSE_VERIFY_KEY";

/// Length of a secret key (an Ed25519 seed) and of a public key, in bytes.
pub const KEY_BYTES: usize = 32;

/// Length of a signature, in bytes.
pub const SIGNATURE_BYTES: usize = 64;

/// The only stage this build signs or accepts.
pub const STAGE_PAPER: &str = "paper";

/// The five fields a manifest signs, plus the signature. Also the exact set `from_json` accepts.
const FIELDS: [&str; 6] = [
    "name",
    "deployment_hash",
    "git_sha",
    "stage",
    "signed_at",
    "signature",
];

/// Why a hex string did not decode to `N` bytes.
enum HexError {
    Length,
    Char,
}

/// Decodes exactly `2 * N` hex characters (either case).
fn decode_hex<const N: usize>(text: &str) -> Result<[u8; N], HexError> {
    let bytes = text.as_bytes();
    if bytes.len() != 2 * N {
        return Err(HexError::Length);
    }
    let (pairs, _) = bytes.as_chunks::<2>();
    let mut out = [0u8; N];
    for (slot, &[hi_char, lo_char]) in out.iter_mut().zip(pairs) {
        let hi = hex_value(hi_char).ok_or(HexError::Char)?;
        let lo = hex_value(lo_char).ok_or(HexError::Char)?;
        *slot = (hi << 4) | lo;
    }
    Ok(out)
}

/// Writes `bytes` as lower-case hex.
fn write_hex(f: &mut fmt::Formatter<'_>, bytes: &[u8]) -> fmt::Result {
    bytes.iter().try_for_each(|b| write!(f, "{b:02x}"))
}

/// Parses a key of `KEY_BYTES` bytes from hex (surrounding whitespace trimmed). The error
/// names `what` and never includes any part of the text.
fn key_from_hex(hex: &str, what: &str) -> Result<[u8; KEY_BYTES], CoreError> {
    decode_hex(hex.trim()).map_err(|e| match e {
        HexError::Length => invalid(format!(
            "{what} must be exactly {KEY_BYTES} bytes ({} hex digits)",
            2 * KEY_BYTES
        )),
        HexError::Char => invalid(format!("{what} contains a non-hex character")),
    })
}

/// The secret signing key. `Debug` never prints its bytes. The key held here is zeroed on
/// drop; the hex text it was parsed from is the caller's and is not.
pub struct SigningKey(ed25519_dalek::SigningKey);

impl SigningKey {
    /// Parses 64 hex digits (upper or lower case, surrounding whitespace trimmed). Any other
    /// length or a non-hex character is `InvalidSigning` with a message that never includes
    /// any part of the key.
    pub fn from_hex(hex: &str) -> Result<Self, CoreError> {
        let seed = key_from_hex(hex, "signing key")?;
        Ok(Self(ed25519_dalek::SigningKey::from_bytes(&seed)))
    }

    /// The public key that verifies this key's signatures.
    #[must_use]
    pub fn verify_key(&self) -> VerifyKey {
        VerifyKey(self.0.verifying_key())
    }
}

impl fmt::Debug for SigningKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SigningKey(..)")
    }
}

/// The public verification key. `Display` is its 64 lower-case hex digits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifyKey(ed25519_dalek::VerifyingKey);

impl VerifyKey {
    /// Parses 64 hex digits (either case, surrounding whitespace trimmed). Any other length,
    /// a non-hex character or bytes that are not a valid public key are `InvalidSigning`.
    pub fn from_hex(hex: &str) -> Result<Self, CoreError> {
        let bytes = key_from_hex(hex, "verification key")?;
        ed25519_dalek::VerifyingKey::from_bytes(&bytes)
            .map(Self)
            .map_err(|_| invalid("verification key is not a valid Ed25519 public key"))
    }
}

impl fmt::Display for VerifyKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_hex(f, self.0.as_bytes())
    }
}

/// An Ed25519 signature. `Display` is its 128 lower-case hex digits.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Signature([u8; SIGNATURE_BYTES]);

impl Signature {
    #[must_use]
    pub const fn from_bytes(bytes: [u8; SIGNATURE_BYTES]) -> Self {
        Self(bytes)
    }
}

impl fmt::Display for Signature {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_hex(f, &self.0)
    }
}

impl fmt::Debug for Signature {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Signature({self})")
    }
}

/// The hash of what an engine trades: SHA-256 of the canonical JSON of
/// `{"benchmark": .., "strategy": .., "symbols": [..sorted..]}`. The order `symbols` is given
/// in does not matter; a duplicate symbol is `InvalidSigning`.
pub fn deployment_hash(
    strategy: &Value,
    symbols: &[&str],
    benchmark: &str,
) -> Result<Hash32, CoreError> {
    let mut sorted = symbols.to_vec();
    sorted.sort_unstable();
    if let Some(pair) = sorted.windows(2).find(|pair| pair[0] == pair[1]) {
        return Err(invalid(format!("duplicate symbol {}", pair[0])));
    }

    let mut map = Map::new();
    map.insert("benchmark".to_owned(), Value::String(benchmark.to_owned()));
    map.insert("strategy".to_owned(), strategy.clone());
    map.insert(
        "symbols".to_owned(),
        Value::Array(
            sorted
                .into_iter()
                .map(|symbol| Value::String(symbol.to_owned()))
                .collect(),
        ),
    );
    Ok(config_hash(&Value::Object(map)))
}

/// A signed statement that a deployment went through review and CI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    pub name: String,
    pub deployment_hash: Hash32,
    /// 40 lower-case hex characters.
    pub git_sha: String,
    pub stage: String,
    /// RFC 3339 UTC to the second, e.g. `2026-11-27T00:00:00Z`.
    pub signed_at: String,
    pub signature: Signature,
}

impl Manifest {
    /// Builds and signs. `stage` other than `STAGE_PAPER`, a `git_sha` that is not 40
    /// lower-case hex characters, an empty `name`, or a `signed_at` that does not parse as
    /// RFC 3339 is `InvalidSigning`.
    pub fn sign(
        key: &SigningKey,
        name: &str,
        deployment_hash: Hash32,
        git_sha: &str,
        stage: &str,
        signed_at: &str,
    ) -> Result<Self, CoreError> {
        if stage != STAGE_PAPER {
            return Err(invalid(format!("stage must be {STAGE_PAPER:?}")));
        }
        if !is_git_sha(git_sha) {
            return Err(invalid("git_sha must be 40 lower-case hex characters"));
        }
        if name.is_empty() {
            return Err(invalid("name must not be empty"));
        }
        if DateTime::parse_from_rfc3339(signed_at).is_err() {
            return Err(invalid("signed_at must be RFC 3339"));
        }

        let message = signed_message(name, &deployment_hash, git_sha, stage, signed_at);
        let signature = Signature(key.0.sign(message.as_bytes()).to_bytes());
        Ok(Self {
            name: name.to_owned(),
            deployment_hash,
            git_sha: git_sha.to_owned(),
            stage: stage.to_owned(),
            signed_at: signed_at.to_owned(),
            signature,
        })
    }

    /// The JSON written to the manifest file: the five fields and `signature`, all strings.
    #[must_use]
    pub fn to_json(&self) -> Value {
        let mut map = signed_fields(
            &self.name,
            &self.deployment_hash,
            &self.git_sha,
            &self.stage,
            &self.signed_at,
        );
        map.insert(
            "signature".to_owned(),
            Value::String(self.signature.to_string()),
        );
        Value::Object(map)
    }

    /// Parses a manifest file's JSON. A missing, mistyped or unknown field is `InvalidSigning`.
    /// Does NOT verify: call `verify`.
    pub fn from_json(value: &Value) -> Result<Self, CoreError> {
        let Value::Object(map) = value else {
            return Err(invalid("manifest must be a JSON object"));
        };
        if let Some(unknown) = map.keys().find(|key| !FIELDS.contains(&key.as_str())) {
            return Err(invalid(format!("unknown field {unknown}")));
        }
        let signature = decode_hex(string_field(map, "signature")?)
            .map(Signature)
            .map_err(|_| {
                invalid(format!(
                    "field signature is not {} hex digits",
                    2 * SIGNATURE_BYTES
                ))
            })?;
        Ok(Self {
            name: string_field(map, "name")?.to_owned(),
            deployment_hash: hash_field(map, "deployment_hash")?,
            git_sha: string_field(map, "git_sha")?.to_owned(),
            stage: string_field(map, "stage")?.to_owned(),
            signed_at: string_field(map, "signed_at")?.to_owned(),
            signature,
        })
    }

    /// Checks the signature under `key` (strict verification: non-canonical signatures and
    /// weak keys are refused). `BadSignature` when it does not verify.
    pub fn verify(&self, key: &VerifyKey) -> Result<(), CoreError> {
        let message = signed_message(
            &self.name,
            &self.deployment_hash,
            &self.git_sha,
            &self.stage,
            &self.signed_at,
        );
        let signature = ed25519_dalek::Signature::from_bytes(&self.signature.0);
        key.0
            .verify_strict(message.as_bytes(), &signature)
            .map_err(|_| CoreError::BadSignature)
    }
}

/// The five signed fields as a JSON object (no `signature`).
fn signed_fields(
    name: &str,
    deployment_hash: &Hash32,
    git_sha: &str,
    stage: &str,
    signed_at: &str,
) -> Map<String, Value> {
    let mut map = Map::new();
    map.insert("name".to_owned(), Value::String(name.to_owned()));
    map.insert(
        "deployment_hash".to_owned(),
        Value::String(deployment_hash.to_string()),
    );
    map.insert("git_sha".to_owned(), Value::String(git_sha.to_owned()));
    map.insert("stage".to_owned(), Value::String(stage.to_owned()));
    map.insert("signed_at".to_owned(), Value::String(signed_at.to_owned()));
    map
}

/// The bytes that are signed: the canonical JSON of the five signed fields.
fn signed_message(
    name: &str,
    deployment_hash: &Hash32,
    git_sha: &str,
    stage: &str,
    signed_at: &str,
) -> String {
    canonical_json(&Value::Object(signed_fields(
        name,
        deployment_hash,
        git_sha,
        stage,
        signed_at,
    )))
}

/// True for exactly 40 lower-case hex characters.
fn is_git_sha(text: &str) -> bool {
    text.len() == 40
        && text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// A `InvalidSigning` error with the given message.
fn invalid(message: impl Into<String>) -> CoreError {
    CoreError::InvalidSigning(message.into())
}

/// The string at `name`, or `InvalidSigning` if it is missing or not a string.
fn string_field<'a>(map: &'a Map<String, Value>, name: &str) -> Result<&'a str, CoreError> {
    match map.get(name) {
        Some(Value::String(text)) => Ok(text),
        Some(_) => Err(invalid(format!("field {name} is not a string"))),
        None => Err(invalid(format!("missing field {name}"))),
    }
}

/// The 64-hex-character hash at `name`, or `InvalidSigning`.
fn hash_field(map: &Map<String, Value>, name: &str) -> Result<Hash32, CoreError> {
    string_field(map, name)?
        .parse::<Hash32>()
        .map_err(|err| invalid(format!("field {name}: {err}")))
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use serde_json::json;

    // Keys. RFC 8032, section 7.1, test 1: this seed has this public key and signs the empty
    // message to this signature.
    const RFC_SEED: &str = "9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60";
    const RFC_PUBLIC: &str = "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a";

    #[test]
    fn verify_key_matches_rfc8032_test_1() {
        let key = SigningKey::from_hex(RFC_SEED).unwrap();
        assert_eq!(key.verify_key().to_string(), RFC_PUBLIC);
        assert_eq!(VerifyKey::from_hex(RFC_PUBLIC).unwrap(), key.verify_key());
    }

    #[test]
    fn from_hex_accepts_upper_case_and_surrounding_whitespace() {
        let text = format!("  {}\n", RFC_SEED.to_uppercase());
        assert_eq!(
            SigningKey::from_hex(&text)
                .unwrap()
                .verify_key()
                .to_string(),
            RFC_PUBLIC
        );
        assert!(VerifyKey::from_hex(&format!(" {}\n", RFC_PUBLIC.to_uppercase())).is_ok());
    }

    #[test]
    fn keys_refuse_a_wrong_length_or_a_non_hex_character() {
        for bad in [
            "a7".repeat(31),
            "a7".repeat(33),
            format!("{}a", "a7".repeat(32)),
            format!("{}zz", "a7".repeat(31)),
            String::new(),
        ] {
            assert!(matches!(
                SigningKey::from_hex(&bad),
                Err(CoreError::InvalidSigning(_))
            ));
            assert!(matches!(
                VerifyKey::from_hex(&bad),
                Err(CoreError::InvalidSigning(_))
            ));
        }
    }

    #[test]
    fn key_material_never_appears_in_errors_or_debug() {
        let debug = format!("{:?}", SigningKey::from_hex(&"a7".repeat(32)).unwrap());
        assert_eq!(debug, "SigningKey(..)");

        for bad in [
            "a7".repeat(31),
            format!("{}a", "a7".repeat(32)),
            format!("{}zz", "a7".repeat(31)),
        ] {
            let err = SigningKey::from_hex(&bad).unwrap_err().to_string();
            assert!(!err.contains("a7"), "error leaked key text: {err}");
        }
    }

    fn strategy() -> Value {
        json!({"lookback": 252, "threshold": "0.5"})
    }

    #[test]
    fn deployment_hash_ignores_symbol_order() {
        let a = deployment_hash(&strategy(), &["MSFT", "AAPL"], "SPY").unwrap();
        let b = deployment_hash(&strategy(), &["AAPL", "MSFT"], "SPY").unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn deployment_hash_ignores_strategy_key_order() {
        let forward: Value = serde_json::from_str(r#"{"a":1,"b":2}"#).unwrap();
        let backward: Value = serde_json::from_str(r#"{"b":2,"a":1}"#).unwrap();
        assert_eq!(
            deployment_hash(&forward, &["AAPL"], "SPY").unwrap(),
            deployment_hash(&backward, &["AAPL"], "SPY").unwrap()
        );
    }

    #[test]
    fn deployment_hash_changes_with_a_strategy_parameter() {
        let base = deployment_hash(&strategy(), &["AAPL"], "SPY").unwrap();
        let changed = json!({"lookback": 253, "threshold": "0.5"});
        assert_ne!(base, deployment_hash(&changed, &["AAPL"], "SPY").unwrap());
    }

    #[test]
    fn deployment_hash_changes_with_a_symbol() {
        let base = deployment_hash(&strategy(), &["AAPL"], "SPY").unwrap();
        assert_ne!(
            base,
            deployment_hash(&strategy(), &["MSFT"], "SPY").unwrap()
        );
        assert_ne!(
            base,
            deployment_hash(&strategy(), &["AAPL", "MSFT"], "SPY").unwrap()
        );
    }

    #[test]
    fn deployment_hash_changes_with_the_benchmark() {
        assert_ne!(
            deployment_hash(&strategy(), &["AAPL"], "SPY").unwrap(),
            deployment_hash(&strategy(), &["AAPL"], "QQQ").unwrap()
        );
    }

    #[test]
    fn deployment_hash_refuses_a_duplicate_symbol() {
        assert!(matches!(
            deployment_hash(&strategy(), &["AAPL", "MSFT", "AAPL"], "SPY"),
            Err(CoreError::InvalidSigning(_))
        ));
    }

    #[test]
    fn deployment_hash_is_config_hash_of_the_canonical_object() {
        let expected = config_hash(&json!({
            "benchmark": "SPY",
            "strategy": {"lookback": 252, "threshold": "0.5"},
            "symbols": ["AAPL", "MSFT"]
        }));
        assert_eq!(
            deployment_hash(&strategy(), &["MSFT", "AAPL"], "SPY").unwrap(),
            expected
        );
    }

    // Manifest signing and verification

    const GIT_SHA: &str = "0123456789abcdef0123456789abcdef01234567";
    const SIGNED_AT: &str = "2026-11-27T00:00:00Z";

    fn key() -> SigningKey {
        SigningKey::from_hex(&"ab".repeat(32)).unwrap()
    }

    fn other_key() -> SigningKey {
        SigningKey::from_hex(&"cd".repeat(32)).unwrap()
    }

    fn deployment() -> Hash32 {
        deployment_hash(&strategy(), &["MSFT", "AAPL"], "SPY").unwrap()
    }

    fn signed() -> Manifest {
        Manifest::sign(
            &key(),
            "trend-v1",
            deployment(),
            GIT_SHA,
            STAGE_PAPER,
            SIGNED_AT,
        )
        .unwrap()
    }

    #[test]
    fn sign_then_verify_passes() {
        assert_eq!(signed().verify(&key().verify_key()), Ok(()));
    }

    #[test]
    fn the_verification_key_parsed_from_its_hex_verifies() {
        let public = VerifyKey::from_hex(&key().verify_key().to_string()).unwrap();
        assert_eq!(signed().verify(&public), Ok(()));
    }

    #[test]
    fn signing_is_deterministic() {
        assert_eq!(signed(), signed());
    }

    #[test]
    fn changing_any_signed_field_breaks_verification() {
        let base = signed();
        let tampered = [
            Manifest {
                name: "trend-v2".to_owned(),
                ..base.clone()
            },
            Manifest {
                deployment_hash: Hash32::from_bytes([1; 32]),
                ..base.clone()
            },
            Manifest {
                git_sha: "fedcba9876543210fedcba9876543210fedcba98".to_owned(),
                ..base.clone()
            },
            Manifest {
                stage: "live".to_owned(),
                ..base.clone()
            },
            Manifest {
                signed_at: "2026-11-28T00:00:00Z".to_owned(),
                ..base.clone()
            },
            Manifest {
                signature: Signature::from_bytes([0; SIGNATURE_BYTES]),
                ..base
            },
        ];
        for (i, manifest) in tampered.iter().enumerate() {
            assert_eq!(
                manifest.verify(&key().verify_key()),
                Err(CoreError::BadSignature),
                "field {i}"
            );
        }
    }

    #[test]
    fn a_different_key_is_bad_signature() {
        assert_eq!(
            signed().verify(&other_key().verify_key()),
            Err(CoreError::BadSignature)
        );
    }

    // to_json and from_json

    #[test]
    fn to_json_then_from_json_round_trips() {
        let manifest = signed();
        let parsed = Manifest::from_json(&manifest.to_json()).unwrap();
        assert_eq!(parsed, manifest);
        assert_eq!(parsed.verify(&key().verify_key()), Ok(()));
    }

    #[test]
    fn to_json_has_six_string_fields() {
        let json = signed().to_json();
        let map = json.as_object().unwrap();
        assert_eq!(map.len(), 6);
        assert!(map.values().all(Value::is_string));
    }

    #[test]
    fn from_json_does_not_verify() {
        let mut json = signed().to_json();
        json["name"] = json!("tampered");
        let parsed = Manifest::from_json(&json).unwrap();
        assert_eq!(
            parsed.verify(&key().verify_key()),
            Err(CoreError::BadSignature)
        );
    }

    #[test]
    fn from_json_refuses_an_unknown_field() {
        let mut json = signed().to_json();
        json["extra"] = json!("x");
        assert!(matches!(
            Manifest::from_json(&json),
            Err(CoreError::InvalidSigning(_))
        ));
    }

    #[test]
    fn from_json_refuses_a_missing_field() {
        let mut json = signed().to_json();
        json.as_object_mut().unwrap().remove("stage");
        assert!(matches!(
            Manifest::from_json(&json),
            Err(CoreError::InvalidSigning(_))
        ));
    }

    #[test]
    fn from_json_refuses_a_non_string_field() {
        let mut json = signed().to_json();
        json["stage"] = json!(1);
        assert!(matches!(
            Manifest::from_json(&json),
            Err(CoreError::InvalidSigning(_))
        ));
    }

    #[test]
    fn from_json_refuses_a_malformed_signature() {
        let mut json = signed().to_json();
        json["signature"] = json!("not-hex");
        assert!(matches!(
            Manifest::from_json(&json),
            Err(CoreError::InvalidSigning(_))
        ));
    }

    #[test]
    fn from_json_refuses_a_non_object() {
        assert!(matches!(
            Manifest::from_json(&json!([])),
            Err(CoreError::InvalidSigning(_))
        ));
    }

    // sign refusals

    fn sign_with(
        stage: &str,
        git_sha: &str,
        name: &str,
        signed_at: &str,
    ) -> Result<Manifest, CoreError> {
        Manifest::sign(&key(), name, deployment(), git_sha, stage, signed_at)
    }

    #[test]
    fn sign_refuses_stage_live() {
        assert!(matches!(
            sign_with("live", GIT_SHA, "trend-v1", SIGNED_AT),
            Err(CoreError::InvalidSigning(_))
        ));
    }

    #[test]
    fn sign_refuses_a_short_git_sha() {
        assert!(matches!(
            sign_with(STAGE_PAPER, &GIT_SHA[..39], "trend-v1", SIGNED_AT),
            Err(CoreError::InvalidSigning(_))
        ));
    }

    #[test]
    fn sign_refuses_an_upper_case_git_sha() {
        assert!(matches!(
            sign_with(STAGE_PAPER, &GIT_SHA.to_uppercase(), "trend-v1", SIGNED_AT),
            Err(CoreError::InvalidSigning(_))
        ));
    }

    #[test]
    fn sign_refuses_a_non_hex_git_sha() {
        let bad = format!("{}g", &GIT_SHA[..39]);
        assert!(matches!(
            sign_with(STAGE_PAPER, &bad, "trend-v1", SIGNED_AT),
            Err(CoreError::InvalidSigning(_))
        ));
    }

    #[test]
    fn sign_refuses_an_empty_name() {
        assert!(matches!(
            sign_with(STAGE_PAPER, GIT_SHA, "", SIGNED_AT),
            Err(CoreError::InvalidSigning(_))
        ));
    }

    #[test]
    fn sign_refuses_a_bad_timestamp() {
        for bad in ["not-a-time", "2026-13-45T00:00:00Z", ""] {
            assert!(
                matches!(
                    sign_with(STAGE_PAPER, GIT_SHA, "trend-v1", bad),
                    Err(CoreError::InvalidSigning(_))
                ),
                "timestamp {bad:?}"
            );
        }
    }

    #[test]
    fn sign_accepts_a_timestamp_with_an_offset() {
        assert!(
            sign_with(
                STAGE_PAPER,
                GIT_SHA,
                "trend-v1",
                "2026-11-27T01:00:00+01:00"
            )
            .is_ok()
        );
    }
}
