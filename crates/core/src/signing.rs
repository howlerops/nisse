//! Signing a deployment: the hash of what the engine will trade (strategy parameters,
//! universe, benchmark) and a keyed MAC over a small manifest naming it. See
//! `docs/phases/phase-7.md`, "Signing and verification".
//!
//! The MAC is HMAC-SHA256 (RFC 2104) with a shared key. Anyone holding the key can both sign
//! and verify; the phase doc records that limitation and the owner's choice of a public-key
//! scheme later.

use std::fmt;

use chrono::DateTime;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use crate::hash::hex_value;
use crate::{CoreError, Hash32, canonical_json, config_hash};

/// Shortest key accepted, in bytes.
pub const MIN_KEY_BYTES: usize = 32;

/// The only stage this build signs or accepts.
pub const STAGE_PAPER: &str = "paper";

/// SHA-256 block size, in bytes (the key is padded to this length for HMAC).
const BLOCK_BYTES: usize = 64;

/// The five fields a manifest signs, plus the MAC itself. Also the exact set `from_json` accepts.
const FIELDS: [&str; 6] = [
    "name",
    "deployment_hash",
    "git_sha",
    "stage",
    "signed_at",
    "mac",
];

/// A signing key. `Debug` prints only its length, never its bytes.
pub struct SigningKey(Vec<u8>);

impl SigningKey {
    /// Parses hex (upper or lower case, surrounding whitespace trimmed). Fewer than
    /// `MIN_KEY_BYTES` bytes, an odd length or a non-hex character is `InvalidSigning`
    /// with a message that never includes any part of the key.
    pub fn from_hex(hex: &str) -> Result<Self, CoreError> {
        let text = hex.trim().as_bytes();
        if text.len() % 2 == 1 {
            return Err(invalid("signing key has an odd number of hex digits"));
        }
        let (pairs, _) = text.as_chunks::<2>();
        let mut bytes = Vec::with_capacity(pairs.len());
        for &[hi_char, lo_char] in pairs {
            let hi = hex_value(hi_char)
                .ok_or_else(|| invalid("signing key contains a non-hex character"))?;
            let lo = hex_value(lo_char)
                .ok_or_else(|| invalid("signing key contains a non-hex character"))?;
            bytes.push((hi << 4) | lo);
        }
        if bytes.len() < MIN_KEY_BYTES {
            return Err(invalid(format!(
                "signing key must be at least {MIN_KEY_BYTES} bytes"
            )));
        }
        Ok(Self(bytes))
    }

    /// The raw key bytes, for the MAC only.
    fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl fmt::Debug for SigningKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SigningKey({} bytes)", self.0.len())
    }
}

/// HMAC-SHA256 (RFC 2104) of `message` under `key`.
#[must_use]
pub fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    // A key longer than the block is hashed first; either way it is zero-padded to a block.
    let mut block = [0u8; BLOCK_BYTES];
    if key.len() > BLOCK_BYTES {
        let digest: [u8; 32] = Sha256::digest(key).into();
        block[..digest.len()].copy_from_slice(&digest);
    } else {
        block[..key.len()].copy_from_slice(key);
    }

    let mut inner_pad = [0u8; BLOCK_BYTES];
    let mut outer_pad = [0u8; BLOCK_BYTES];
    for ((ipad, opad), &k) in inner_pad.iter_mut().zip(outer_pad.iter_mut()).zip(&block) {
        *ipad = k ^ 0x36;
        *opad = k ^ 0x5c;
    }

    let mut inner = Sha256::new();
    inner.update(inner_pad);
    inner.update(message);
    let inner_digest: [u8; 32] = inner.finalize().into();

    let mut outer = Sha256::new();
    outer.update(outer_pad);
    outer.update(inner_digest);
    outer.finalize().into()
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
    pub mac: Hash32,
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

        let fields = signed_fields(name, &deployment_hash, git_sha, stage, signed_at);
        let mac = compute_mac(key, fields);
        Ok(Self {
            name: name.to_owned(),
            deployment_hash,
            git_sha: git_sha.to_owned(),
            stage: stage.to_owned(),
            signed_at: signed_at.to_owned(),
            mac: Hash32::from_bytes(mac),
        })
    }

    /// The JSON written to the manifest file: the five fields and `mac`, all strings.
    #[must_use]
    pub fn to_json(&self) -> Value {
        let mut map = signed_fields(
            &self.name,
            &self.deployment_hash,
            &self.git_sha,
            &self.stage,
            &self.signed_at,
        );
        map.insert("mac".to_owned(), Value::String(self.mac.to_string()));
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
        Ok(Self {
            name: string_field(map, "name")?.to_owned(),
            deployment_hash: hash_field(map, "deployment_hash")?,
            git_sha: string_field(map, "git_sha")?.to_owned(),
            stage: string_field(map, "stage")?.to_owned(),
            signed_at: string_field(map, "signed_at")?.to_owned(),
            mac: hash_field(map, "mac")?,
        })
    }

    /// Checks the MAC under `key` in constant time. `BadSignature` when it does not match.
    pub fn verify(&self, key: &SigningKey) -> Result<(), CoreError> {
        let fields = signed_fields(
            &self.name,
            &self.deployment_hash,
            &self.git_sha,
            &self.stage,
            &self.signed_at,
        );
        let expected = compute_mac(key, fields);
        if constant_time_eq(&expected, self.mac.as_bytes()) {
            Ok(())
        } else {
            Err(CoreError::BadSignature)
        }
    }
}

/// The five signed fields as a JSON object (no `mac`).
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

/// HMAC of the canonical JSON of `fields`.
fn compute_mac(key: &SigningKey, fields: Map<String, Value>) -> [u8; 32] {
    let text = canonical_json(&Value::Object(fields));
    hmac_sha256(key.as_bytes(), text.as_bytes())
}

/// Compares two MACs without an early return: every byte pair is folded into one value.
fn constant_time_eq(a: &[u8; 32], b: &[u8; 32]) -> bool {
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
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
mod tests {
    use super::*;
    use serde_json::json;

    fn hmac_hex(key: &[u8], message: &[u8]) -> String {
        hmac_sha256(key, message)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    // hmac_sha256 (RFC 4231)

    #[test]
    fn hmac_rfc4231_case_1() {
        assert_eq!(
            hmac_hex(&[0x0b; 20], b"Hi There"),
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
        );
    }

    #[test]
    fn hmac_rfc4231_case_2() {
        assert_eq!(
            hmac_hex(b"Jefe", b"what do ya want for nothing?"),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
    }

    #[test]
    fn hmac_rfc4231_case_3() {
        assert_eq!(
            hmac_hex(&[0xaa; 20], &[0xdd; 50]),
            "773ea91e36800e46854db8ebd09181a72959098b3ef8c122d9635514ced565fe"
        );
    }

    #[test]
    fn hmac_rfc4231_case_4() {
        let key: Vec<u8> = (0x01..=0x19).collect();
        assert_eq!(
            hmac_hex(&key, &[0xcd; 50]),
            "82558a389a443c0ea4cc819899f2083a85f0faa3e578f8077a2e3ff46729665b"
        );
    }

    #[test]
    fn hmac_rfc4231_case_6_uses_hashed_long_key() {
        let key = vec![0xaa; 131];
        assert_eq!(
            hmac_hex(
                &key,
                b"Test Using Larger Than Block-Size Key - Hash Key First"
            ),
            "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54"
        );
    }

    #[test]
    fn hmac_key_of_exactly_block_size_is_not_hashed() {
        // 64 bytes is used as-is; 65 bytes takes the hashed path. The two must differ.
        let key64 = vec![0x11; 64];
        let mut key65 = key64.clone();
        key65.push(0x11);
        assert_ne!(hmac_sha256(&key64, b"m"), hmac_sha256(&key65, b"m"));
    }

    #[test]
    fn hmac_changes_with_the_message() {
        assert_ne!(hmac_sha256(b"k", b"a"), hmac_sha256(b"k", b"b"));
    }

    // SigningKey

    #[test]
    fn from_hex_accepts_32_bytes_in_either_case() {
        assert!(SigningKey::from_hex(&"ab".repeat(32)).is_ok());
        assert!(SigningKey::from_hex(&"AB".repeat(32)).is_ok());
        assert!(SigningKey::from_hex(&format!("  {}\n", "Ab".repeat(32))).is_ok());
    }

    #[test]
    fn from_hex_refuses_31_bytes() {
        assert!(matches!(
            SigningKey::from_hex(&"ab".repeat(31)),
            Err(CoreError::InvalidSigning(_))
        ));
    }

    #[test]
    fn from_hex_refuses_odd_length() {
        assert!(matches!(
            SigningKey::from_hex(&format!("{}a", "ab".repeat(32))),
            Err(CoreError::InvalidSigning(_))
        ));
    }

    #[test]
    fn from_hex_refuses_non_hex() {
        let key = format!("{}zz", "a7".repeat(31));
        assert!(matches!(
            SigningKey::from_hex(&key),
            Err(CoreError::InvalidSigning(_))
        ));
    }

    #[test]
    fn key_material_never_appears_in_errors_or_debug() {
        let distinctive = "a7".repeat(64);
        let debug = format!("{:?}", SigningKey::from_hex(&distinctive).unwrap());
        assert_eq!(debug, "SigningKey(64 bytes)");
        assert!(!debug.contains("a7a7"));

        for bad in [
            "a7".repeat(31),
            format!("{}a", "a7".repeat(32)),
            format!("{}zz", "a7".repeat(31)),
        ] {
            let err = SigningKey::from_hex(&bad).unwrap_err().to_string();
            assert!(!err.contains("a7"), "error leaked key text: {err}");
        }
    }

    // deployment_hash

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
        assert_eq!(signed().verify(&key()), Ok(()));
    }

    #[test]
    fn mac_is_hmac_of_the_canonical_signed_fields() {
        let expected = hmac_sha256(
            key().as_bytes(),
            canonical_json(&json!({
                "name": "trend-v1",
                "deployment_hash": deployment().to_string(),
                "git_sha": GIT_SHA,
                "stage": "paper",
                "signed_at": SIGNED_AT,
            }))
            .as_bytes(),
        );
        assert_eq!(signed().mac, Hash32::from_bytes(expected));
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
                mac: Hash32::from_bytes([0; 32]),
                ..base
            },
        ];
        for (i, manifest) in tampered.iter().enumerate() {
            assert_eq!(
                manifest.verify(&key()),
                Err(CoreError::BadSignature),
                "field {i}"
            );
        }
    }

    #[test]
    fn a_different_key_is_bad_signature() {
        assert_eq!(signed().verify(&other_key()), Err(CoreError::BadSignature));
    }

    // to_json and from_json

    #[test]
    fn to_json_then_from_json_round_trips() {
        let manifest = signed();
        let parsed = Manifest::from_json(&manifest.to_json()).unwrap();
        assert_eq!(parsed, manifest);
        assert_eq!(parsed.verify(&key()), Ok(()));
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
        assert_eq!(parsed.verify(&key()), Err(CoreError::BadSignature));
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
    fn from_json_refuses_a_malformed_hash() {
        let mut json = signed().to_json();
        json["mac"] = json!("not-hex");
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
