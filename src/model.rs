//! The static rANS probability model.
//!
//! Every lane record selects a model `q` (0..=15) and a symbol width `w`
//! (1..=8, so `n = 2^w` symbols). The frequency table for `(q, w)` is not
//! transmitted: it is generated from a small parametric law.
//!
//! * Probabilities are 16-bit: every table sums to `M = 65536`.
//! * Slot 0 (frequency 1) is the FIN symbol. Real symbols `v = 0..n-1` follow,
//!   so `cum[0] = 1` and `cum[n] = 65536`.
//! * `q = 0`: uniform. Symbol 0 has `2^(16-w) - 1` slots, every other symbol
//!   `2^(16-w)` slots.
//! * `q >= 1`: truncated geometric law `F(v) = (1 - rho^v) / (1 - rho^n)` with
//!   `rho = exp(-lambda_q / n)`, quantised by power-of-two buckets
//!   `{0}, {1}, {2,3}, {4..7}, ...`. Each bucket gets one per-symbol
//!   frequency; buckets are fixed from the top down and keep at least one slot
//!   per symbol (see [`geometric_frequencies`]).
//! * `q = 15` uses a very large lambda: the table is saturated (symbol 0 takes
//!   almost all the mass, every other symbol has frequency 1).

use crate::error::{Error, Result};
use std::sync::OnceLock;

/// Probability scale: tables sum to `M`.
pub const M: u32 = 1 << 16;
/// Lower bound of the normalised rANS state interval `[L, 256 * L)`.
pub const L: u64 = 1 << 24;
/// Largest model index carried by a record header.
pub const MAX_Q: u32 = 15;
/// Largest symbol width supported by the model.
pub const MAX_WIDTH: u32 = 8;

/// `lambda_q` for `q = 1..=15` (index `q - 1`). Values were fitted on data so
/// that the generated integer tables decode every observed record exactly.
pub const LAMBDA: [f64; 15] = [
    0.7399434,  // q = 1
    1.4962494,  // q = 2
    2.30715745, // q = 3
    3.22101565, // q = 4
    4.3073742,  // q = 5
    5.6759508,  // q = 6
    7.50811,    // q = 7
    10.1124375, // q = 8
    14.03737,   // q = 9
    20.35237,   // q = 10
    31.4462,    // q = 11
    53.67977,   // q = 12
    109.2579,   // q = 13
    328.076,    // q = 14
    3000.0,     // q = 15 (saturated)
];

/// Power-of-two buckets for width `w`: `(start, size)` pairs
/// `(0,1), (1,1), (2,2), (4,4), ..., (n/2, n/2)`; there are `w + 1` of them.
pub fn buckets(w: u32) -> Vec<(u32, u32)> {
    let mut b = vec![(0u32, 1u32), (1, 1)];
    let mut k = 1;
    while (1u32 << k) < (1u32 << w) {
        b.push((1 << k, 1 << k));
        k += 1;
    }
    b.truncate(w as usize + 1);
    b
}

/// Per-symbol frequency of every bucket for the geometric law
/// (`lambda`, width `w`).
///
/// 1. Float boundary at each bucket start: `B_k = M * (1 - rho^start_k) / (1 - rho^n)`,
///    rounded half-to-even; the first boundary is forced to 1 (FIN slot) and
///    the last to `M`.
/// 2. From the last bucket down to bucket 1: clamp the boundary so that the
///    bucket keeps at least one slot per symbol, take the per-symbol
///    frequency `f = max(1, round_half_even(mass / size))`, and move the
///    boundary to `B_{k+1} - f * size`.
/// 3. Bucket 0 (symbol 0) receives what is left.
pub fn geometric_frequencies(lambda: f64, w: u32) -> Vec<i64> {
    let n = (1u64 << w) as f64;
    let rho = (-lambda / n).exp();
    let bs = buckets(w);
    let z = 1.0 - rho.powf(n);
    let mut bound: Vec<i64> = bs
        .iter()
        .map(|&(start, _)| {
            let x = M as f64 * (1.0 - rho.powf(start as f64)) / z;
            x.round_ties_even() as i64
        })
        .collect();
    bound.push(M as i64);
    bound[0] = 1;
    let last = bound.len() - 1;
    bound[last] = M as i64;
    for k in (1..bs.len()).rev() {
        let size = bs[k].1 as i64;
        bound[k] = bound[k].min(bound[k + 1] - size);
        let f = (((bound[k + 1] - bound[k]) as f64) / size as f64).round_ties_even() as i64;
        let f = f.max(1);
        bound[k] = bound[k + 1] - f * size;
    }
    (0..bs.len())
        .map(|k| (bound[k + 1] - bound[k]).div_euclid(bs[k].1 as i64))
        .collect()
}

/// One frequency table: cumulative frequencies of the FIN slot and the `n`
/// real symbols.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelTable {
    /// `cum[v]` = first slot of symbol `v`; `cum[n] = M`; `cum[0] = 1`.
    pub cum: Vec<u32>,
}

impl ModelTable {
    /// Table for model `q` and width `w`.
    pub fn new(q: u32, w: u32) -> Result<Self> {
        if q > MAX_Q || w == 0 || w > MAX_WIDTH {
            return Err(Error::Model(format!("no table for q = {q}, w = {w}")));
        }
        let n = 1u32 << w;
        let cum: Vec<u32> = if q == 0 {
            let mut c = vec![1u32];
            c.extend((1..=n).map(|v| v << (16 - w)));
            c
        } else {
            let freqs = geometric_frequencies(LAMBDA[q as usize - 1], w);
            let mut c = vec![1i64];
            for ((_, size), f) in buckets(w).into_iter().zip(freqs) {
                for _ in 0..size {
                    let next = c[c.len() - 1] + f;
                    c.push(next);
                }
            }
            if c.iter().any(|&v| !(0..=M as i64).contains(&v)) {
                return Err(Error::Model(format!(
                    "q = {q}, w = {w}: frequency out of range"
                )));
            }
            c.into_iter().map(|v| v as u32).collect()
        };
        let t = ModelTable { cum };
        t.validate(n)
            .map_err(|m| Error::Model(format!("q = {q}, w = {w}: {m}")))?;
        Ok(t)
    }

    fn validate(&self, n: u32) -> std::result::Result<(), String> {
        if self.cum.len() != n as usize + 1 {
            return Err(format!("{} entries instead of {}", self.cum.len(), n + 1));
        }
        if self.cum[0] != 1 {
            return Err("FIN slot must have frequency 1".into());
        }
        if self.cum[n as usize] != M {
            return Err(format!(
                "table sums to {} instead of {M}",
                self.cum[n as usize]
            ));
        }
        if self.cum.windows(2).any(|p| p[1] <= p[0]) {
            return Err("every symbol needs a frequency of at least 1".into());
        }
        Ok(())
    }

    /// Number of real symbols.
    pub fn symbols(&self) -> usize {
        self.cum.len() - 1
    }

    /// Frequency of symbol `v`.
    pub fn freq(&self, v: usize) -> u32 {
        self.cum[v + 1] - self.cum[v]
    }

    /// Symbol owning `slot` (`1..M`); `None` for the FIN slot 0.
    pub fn lookup(&self, slot: u32) -> Option<usize> {
        if slot == 0 || slot >= M {
            return None;
        }
        // Largest v with cum[v] <= slot.
        let v = self.cum.partition_point(|&c| c <= slot) - 1;
        Some(v)
    }
}

/// All 16 x 8 model tables.
#[derive(Debug, Clone)]
pub struct Models {
    tables: Vec<ModelTable>, // index q * 8 + (w - 1)
}

impl Models {
    /// Generates and validates every table.
    pub fn build() -> Result<Self> {
        let mut tables = Vec::with_capacity(16 * 8);
        for q in 0..=MAX_Q {
            for w in 1..=MAX_WIDTH {
                tables.push(ModelTable::new(q, w)?);
            }
        }
        Ok(Models { tables })
    }

    /// Table for `(q, w)`; the caller guarantees `q <= 15` and `1 <= w <= 8`.
    pub fn get(&self, q: u32, w: u32) -> &ModelTable {
        &self.tables[(q * MAX_WIDTH + (w - 1)) as usize]
    }
}

static MODELS: OnceLock<Result<Models>> = OnceLock::new();

/// Shared, lazily built model tables.
pub fn models() -> Result<&'static Models> {
    MODELS
        .get_or_init(Models::build)
        .as_ref()
        .map_err(|e| e.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_table_sums_to_65536() {
        let m = models().unwrap();
        for q in 0..=MAX_Q {
            for w in 1..=MAX_WIDTH {
                let t = m.get(q, w);
                assert_eq!(t.cum[0], 1);
                assert_eq!(*t.cum.last().unwrap(), 65536, "q={q} w={w}");
                assert_eq!(t.symbols(), 1 << w);
                let total: u32 = 1 + (0..t.symbols()).map(|v| t.freq(v)).sum::<u32>();
                assert_eq!(total, 65536);
                assert!((0..t.symbols()).all(|v| t.freq(v) >= 1));
            }
        }
    }

    #[test]
    fn uniform_model_has_fin_slot() {
        let t = ModelTable::new(0, 3).unwrap();
        assert_eq!(
            t.cum,
            vec![1, 8192, 16384, 24576, 32768, 40960, 49152, 57344, 65536]
        );
        assert_eq!(t.freq(0), 8191);
        assert_eq!(t.freq(1), 8192);
        assert_eq!(t.lookup(0), None);
        assert_eq!(t.lookup(1), Some(0));
        assert_eq!(t.lookup(8191), Some(0));
        assert_eq!(t.lookup(8192), Some(1));
        assert_eq!(t.lookup(65535), Some(7));
    }

    #[test]
    fn geometric_tables_are_monotone_by_bucket() {
        let m = models().unwrap();
        for q in 1..=MAX_Q {
            for w in 1..=MAX_WIDTH {
                let t = m.get(q, w);
                let mut prev = u32::MAX;
                for (start, size) in buckets(w) {
                    let f = t.freq(start as usize);
                    for v in start..start + size {
                        assert_eq!(t.freq(v as usize), f, "bucket shares one frequency");
                    }
                    assert!(f <= prev, "q={q} w={w}: frequencies decrease");
                    prev = f;
                }
            }
        }
    }

    #[test]
    fn known_table_values() {
        // (12, 5): per-symbol frequency of each bucket, documented in docs/SPEC.md.
        assert_eq!(
            geometric_frequencies(LAMBDA[11], 5),
            vec![53290, 9957, 1104, 14, 1, 1]
        );
        // q = 15 is saturated: everything but symbol 0 has frequency 1.
        let t = ModelTable::new(15, 4).unwrap();
        assert_eq!(t.freq(0), 65536 - 1 - 15);
        assert!((1..16).all(|v| t.freq(v) == 1));
    }

    /// Fingerprint of all 128 tables. Guards against floating-point
    /// differences between platforms (exp/pow/rounding).
    #[test]
    fn table_fingerprint() {
        let m = models().unwrap();
        let mut h: u64 = 0xcbf29ce484222325;
        for q in 0..=MAX_Q {
            for w in 1..=MAX_WIDTH {
                for &c in &m.get(q, w).cum {
                    for b in c.to_le_bytes() {
                        h ^= b as u64;
                        h = h.wrapping_mul(0x100000001b3);
                    }
                }
            }
        }
        assert_eq!(h, TABLE_FINGERPRINT, "got 0x{h:016x}");
    }

    const TABLE_FINGERPRINT: u64 = 0x6defe80b3926e964;
}
