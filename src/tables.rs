//! Runtime table file (JSON) for the learned BC1 constants.
//!
//! BC1 needs two learned tables that are *not* shipped with this crate:
//!
//! * `history`: 255 endpoint pairs that stand for the virtual blocks
//!   `-1 ... -255` before the start of every chunk (back-references that
//!   reach before block 0 read them);
//! * `quadrant_codes`: the mapping from an index-lane symbol (0..=255) to the
//!   byte of four 2-bit selectors of a 2 x 2 quadrant.
//!
//! Format (see `docs/SPEC.md`, section "Table file"):
//!
//! ```json
//! {
//!   "format": "bcpack-tables",
//!   "version": 1,
//!   "bc1": {
//!     "history": [[r0, g0, b0, r1, g1, b1], null, ...],
//!     "quadrant_codes": [170, 255, 0, 85, ...]
//!   }
//! }
//! ```
//!
//! `history[k]` is the entry for virtual block `-(k + 1)`; `null` marks an
//! unknown entry (decoding fails only if a block actually references it).
//! R and B are 5-bit values (0..=31), G is 6-bit (0..=63).

use crate::error::{Error, Result};
use crate::json::{self, Value};

/// Identifier stored in the `format` member.
pub const FORMAT_NAME: &str = "bcpack-tables";
/// Current format version.
pub const FORMAT_VERSION: u64 = 1;
/// Number of history entries.
pub const HISTORY_LEN: usize = 255;

/// Learned BC1 tables.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bc1Tables {
    /// `history[k]` = endpoints `[r0, g0, b0, r1, g1, b1]` of virtual block `-(k+1)`.
    pub history: Vec<Option<[u8; 6]>>,
    /// Symbol -> quadrant selector byte. Bits `2p..2p+1` hold the selector of
    /// pixel `p` of the quadrant, in the order top-left, top-right,
    /// bottom-left, bottom-right.
    pub quadrant_codes: [u8; 256],
}

impl Default for Bc1Tables {
    /// Empty tables: unknown history, identity quadrant mapping.
    fn default() -> Self {
        let mut q = [0u8; 256];
        for (i, v) in q.iter_mut().enumerate() {
            *v = i as u8;
        }
        Bc1Tables {
            history: vec![None; HISTORY_LEN],
            quadrant_codes: q,
        }
    }
}

const ENDPOINT_MOD: [u32; 6] = [32, 64, 32, 32, 64, 32];

impl Bc1Tables {
    /// Checks ranges and that `quadrant_codes` is a permutation of 0..=255.
    pub fn validate(&self) -> Result<()> {
        if self.history.len() != HISTORY_LEN {
            return Err(Error::Tables(format!(
                "history has {} entries instead of {HISTORY_LEN}",
                self.history.len()
            )));
        }
        for (k, e) in self.history.iter().enumerate() {
            if let Some(e) = e {
                for c in 0..6 {
                    if e[c] as u32 >= ENDPOINT_MOD[c] {
                        return Err(Error::Tables(format!(
                            "history[{k}] channel {c} = {} is out of range",
                            e[c]
                        )));
                    }
                }
            }
        }
        let mut seen = [false; 256];
        for &c in &self.quadrant_codes {
            if std::mem::replace(&mut seen[c as usize], true) {
                return Err(Error::Tables(format!(
                    "quadrant_codes is not a permutation (code {c} appears twice)"
                )));
            }
        }
        Ok(())
    }

    /// Parses and validates a table file.
    pub fn from_json(text: &str) -> Result<Self> {
        let v = json::parse(text)?;
        if v.get("format").and_then(Value::as_str) != Some(FORMAT_NAME) {
            return Err(Error::Tables(format!(
                "\"format\" must be \"{FORMAT_NAME}\""
            )));
        }
        match v.get("version").and_then(Value::as_u64) {
            Some(FORMAT_VERSION) => {}
            other => {
                return Err(Error::Tables(format!("unsupported version {other:?}")));
            }
        }
        let bc1 = v
            .get("bc1")
            .ok_or_else(|| Error::Tables("missing \"bc1\"".into()))?;
        let hist = bc1
            .get("history")
            .and_then(Value::as_array)
            .ok_or_else(|| Error::Tables("missing \"bc1.history\" array".into()))?;
        let mut history = Vec::with_capacity(hist.len());
        for (k, e) in hist.iter().enumerate() {
            match e {
                Value::Null => history.push(None),
                Value::Array(a) if a.len() == 6 => {
                    let mut out = [0u8; 6];
                    for c in 0..6 {
                        out[c] = a[c].as_u64().filter(|&x| x < 256).ok_or_else(|| {
                            Error::Tables(format!("history[{k}][{c}] is not a small integer"))
                        })? as u8;
                    }
                    history.push(Some(out));
                }
                _ => {
                    return Err(Error::Tables(format!(
                        "history[{k}] must be null or an array of 6 integers"
                    )))
                }
            }
        }
        let q = bc1
            .get("quadrant_codes")
            .and_then(Value::as_array)
            .ok_or_else(|| Error::Tables("missing \"bc1.quadrant_codes\" array".into()))?;
        if q.len() != 256 {
            return Err(Error::Tables(format!(
                "quadrant_codes has {} entries instead of 256",
                q.len()
            )));
        }
        let mut quadrant_codes = [0u8; 256];
        for (i, e) in q.iter().enumerate() {
            quadrant_codes[i] =
                e.as_u64().filter(|&x| x < 256).ok_or_else(|| {
                    Error::Tables(format!("quadrant_codes[{i}] is not in 0..=255"))
                })? as u8;
        }
        let t = Bc1Tables {
            history,
            quadrant_codes,
        };
        t.validate()?;
        Ok(t)
    }

    /// Reads a table file from disk.
    pub fn load(path: &std::path::Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| Error::Io(format!("{}: {e}", path.display())))?;
        Self::from_json(&text)
    }

    /// Serialises the tables (one history entry per line).
    pub fn to_json(&self) -> String {
        let mut s = String::new();
        s.push_str("{\n");
        s.push_str(&format!("  \"format\": {},\n", json::quote(FORMAT_NAME)));
        s.push_str(&format!("  \"version\": {FORMAT_VERSION},\n"));
        s.push_str("  \"bc1\": {\n    \"history\": [\n");
        for (k, e) in self.history.iter().enumerate() {
            let item = match e {
                None => "null".to_string(),
                Some(e) => format!(
                    "[{}, {}, {}, {}, {}, {}]",
                    e[0], e[1], e[2], e[3], e[4], e[5]
                ),
            };
            let sep = if k + 1 < self.history.len() { "," } else { "" };
            s.push_str(&format!("      {item}{sep}\n"));
        }
        s.push_str("    ],\n    \"quadrant_codes\": [\n");
        for (row, chunk) in self.quadrant_codes.chunks(16).enumerate() {
            let items: Vec<String> = chunk.iter().map(|c| c.to_string()).collect();
            let sep = if row < 15 { "," } else { "" };
            s.push_str(&format!("      {}{sep}\n", items.join(", ")));
        }
        s.push_str("    ]\n  }\n}\n");
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_round_trip() {
        let mut t = Bc1Tables::default();
        t.history[0] = Some([1, 2, 3, 4, 5, 6]);
        t.history[254] = Some([31, 63, 31, 0, 0, 0]);
        t.quadrant_codes.swap(3, 200);
        let back = Bc1Tables::from_json(&t.to_json()).unwrap();
        assert_eq!(back, t);
    }

    #[test]
    fn rejects_bad_tables() {
        let good = Bc1Tables::default().to_json();
        assert!(Bc1Tables::from_json(&good.replace("bcpack-tables", "other")).is_err());
        assert!(Bc1Tables::from_json(&good.replace("\"version\": 1", "\"version\": 2")).is_err());
        let mut t = Bc1Tables::default();
        t.history[5] = Some([32, 0, 0, 0, 0, 0]);
        assert!(Bc1Tables::from_json(&t.to_json()).is_err());
        let mut t = Bc1Tables::default();
        t.quadrant_codes[1] = 0;
        assert!(Bc1Tables::from_json(&t.to_json()).is_err());
        let mut t = Bc1Tables::default();
        t.history.pop();
        assert!(Bc1Tables::from_json(&t.to_json()).is_err());
    }
}
