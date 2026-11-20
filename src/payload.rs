//! The record embedded in both the metadata and the steganographic channel.

use crate::stego;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Payload {
    pub v: u8,
    pub id: String,
    pub unix: u64,
    pub utc: String,
    pub file: String,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl Payload {
    pub fn new(id: String, unix: u64, file: String, text: String, note: Option<String>) -> Self {
        Self {
            v: 1,
            id,
            unix,
            utc: crate::datetime::iso8601(unix),
            file,
            text,
            note,
        }
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("payload is serializable")
    }

    pub fn from_json(text: &str) -> Result<Self, String> {
        serde_json::from_str(text).map_err(|e| format!("invalid payload JSON: {e}"))
    }
}

/// Deterministic-but-unique-enough 64-bit id derived from time, filename and key.
pub fn make_id(unix: u64, file: &str, key: &str) -> String {
    let seed = stego::seed_from_key(key);
    let x = stego::splitmix64(
        seed ^ unix.rotate_left(17) ^ stego::fnv1a(file.as_bytes()).rotate_left(31),
    );
    format!("{x:016x}")
}

/// Expand `{file}`, `{date}`, `{time}`, `{id}` placeholders in the visible text.
pub fn resolve_text(template: &str, file: &str, id: &str, unix: u64) -> String {
    let stem = std::path::Path::new(file)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(file);
    template
        .replace("{file}", stem)
        .replace("{date}", &crate::datetime::date_only(unix))
        .replace("{time}", &crate::datetime::iso8601(unix))
        .replace("{id}", id)
}
