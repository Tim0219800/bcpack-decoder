//! Deriving the BC1 table file from your own data.
//!
//! The decoder needs two learned tables (see [`crate::tables`]). They are not
//! shipped; this module rebuilds them from samples you provide. Two kinds of
//! reference are supported:
//!
//! * **`blocks`**: the exact BC1 blocks that a chunk decodes to (for example
//!   the same texture stored elsewhere without BCPack). Both tables follow
//!   exactly: every index-lane symbol votes for the quadrant code it must
//!   produce, and every block whose back-reference reaches before the chunk
//!   start votes for the history entry it needs (`entry = endpoint - delta`).
//! * **`quads`**: an approximate RGB reference, 12 bytes per block: the colour
//!   of each 2 x 2 quadrant (top-left, top-right, bottom-left, bottom-right;
//!   R, G, B each), in stream order. The next mip level of a texture whose
//!   chunk holds exactly one mip gives this directly (one next-mip pixel per
//!   quadrant). History entries are fitted by least squares: for each entry
//!   and each colour channel, all 32 x 32 (or 64 x 64) endpoint pairs are
//!   scored against every block whose reference chain starts at that entry,
//!   summed over all samples, and the minimum is kept. A quadrant colour is
//!   modelled as `c0 + (c1 - c0) * k / 12`, where `k` is the sum of the
//!   palette ranks of its four selectors (ranks 0, 3, 1, 2 for selectors
//!   0, 1, 2, 3).
//!
//! With `quads` the quadrant mapping is taken from `--tables` if given;
//! otherwise it is estimated from a corpus of plain (non-BCPack) BC1 blocks:
//! symbols are first matched to codes by frequency, constrained by the
//! measured mean weight of each symbol, then pairs of symbols with the same
//! weight are swapped while the likelihood of horizontal and vertical
//! quadrant neighbours (bigrams, measured on the corpus) increases.
//! This estimate is statistical: frequent symbols come out right, rare ones
//! may not. Exact `blocks` pairs, when available, are always preferable.

use crate::bc1::{block_endpoints, block_quadrants, unfold_distance, ENDPOINT_MOD};
use crate::error::{Error, Result};
use crate::lanes::{decode_lanes, CodecParams};
use crate::tables::{Bc1Tables, HISTORY_LEN};
use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};

/// Kind of reference given for each sample.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefKind {
    /// Exact BC1 blocks, `size` bytes.
    Blocks,
    /// 2 x 2 quadrant colours, 12 bytes per block (`size / 8 * 12` bytes).
    Quads,
}

/// Options of the `learn-tables` command.
#[derive(Debug, Clone)]
pub struct LearnOptions {
    /// Sample list: one `CHUNK SIZE REFERENCE` line per sample.
    pub list: PathBuf,
    pub kind: RefKind,
    /// Existing table file: supplies the quadrant mapping in `quads` mode and
    /// the entries that the samples leave unobserved.
    pub initial: Option<PathBuf>,
    /// List of plain BC1 block files (only for `quads` without `initial`).
    pub corpus: Option<PathBuf>,
}

/// Summary printed by the CLI.
#[derive(Debug, Default, Clone)]
pub struct Report {
    pub samples: usize,
    pub skipped: Vec<String>,
    pub history_learned: usize,
    pub history_conflicts: usize,
    pub symbols_observed: usize,
    pub symbol_conflicts: usize,
    pub self_check: Option<(usize, usize)>,
    pub notes: Vec<String>,
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "samples used: {}", self.samples)?;
        if !self.skipped.is_empty() {
            writeln!(f, "samples skipped: {}", self.skipped.len())?;
            for s in self.skipped.iter().take(10) {
                writeln!(f, "  {s}")?;
            }
        }
        writeln!(
            f,
            "history entries learned: {} of {HISTORY_LEN}",
            self.history_learned
        )?;
        if self.history_conflicts > 0 {
            writeln!(
                f,
                "history entries with conflicting votes: {}",
                self.history_conflicts
            )?;
        }
        writeln!(
            f,
            "quadrant symbols observed: {} of 256",
            self.symbols_observed
        )?;
        if self.symbol_conflicts > 0 {
            writeln!(
                f,
                "quadrant symbols with conflicting votes: {}",
                self.symbol_conflicts
            )?;
        }
        if let Some((ok, n)) = self.self_check {
            writeln!(
                f,
                "self-check: {ok} of {n} samples rebuild their reference exactly"
            )?;
        }
        for n in &self.notes {
            writeln!(f, "note: {n}")?;
        }
        Ok(())
    }
}

/// One sample of the list.
#[derive(Debug, Clone)]
pub struct SampleSpec {
    pub chunk: PathBuf,
    pub size: usize,
    pub reference: PathBuf,
}

/// Reads a sample list. Paths are relative to the list's folder; fields are
/// separated by tabs if the line has any, otherwise by whitespace; `#`
/// starts a comment line.
pub fn read_list(path: &Path) -> Result<Vec<SampleSpec>> {
    let text =
        std::fs::read_to_string(path).map_err(|e| Error::Io(format!("{}: {e}", path.display())))?;
    let base = path.parent().unwrap_or(Path::new("."));
    let mut out = Vec::new();
    for (n, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let f: Vec<&str> = if line.contains('\t') {
            line.split('\t').map(str::trim).collect()
        } else {
            line.split_whitespace().collect()
        };
        if f.len() != 3 {
            return Err(Error::InvalidParams(format!(
                "{}:{}: expected `CHUNK SIZE REFERENCE`",
                path.display(),
                n + 1
            )));
        }
        let size = f[1].parse().map_err(|_| {
            Error::InvalidParams(format!("{}:{}: bad size {:?}", path.display(), n + 1, f[1]))
        })?;
        out.push(SampleSpec {
            chunk: base.join(f[0]),
            size,
            reference: base.join(f[2]),
        });
    }
    Ok(out)
}

fn read_file(p: &Path) -> Result<Vec<u8>> {
    std::fs::read(p).map_err(|e| Error::Io(format!("{}: {e}", p.display())))
}

/// Root and accumulated deltas of each block's reference chain.
/// `root[i] = 0`: the chain starts from the zero predictor; `root[i] = s > 0`:
/// it starts from history entry `-s`. `acc[i]` is the (unreduced) sum of the
/// zigzag deltas from the root.
pub fn chains(lanes: &[Vec<u8>]) -> (Vec<usize>, Vec<[i32; 6]>) {
    let nb = lanes[0].len();
    let mut root = vec![0usize; nb];
    let mut acc = vec![[0i32; 6]; nb];
    for i in 0..nb {
        let mut dl = [0i32; 6];
        for c in 0..6 {
            dl[c] = crate::bc1::zigzag_decode(lanes[2 + c][i]);
        }
        let d = unfold_distance(lanes[0][i]);
        if d == 0 {
            acc[i] = dl;
        } else if d > i {
            root[i] = d - i;
            acc[i] = dl;
        } else {
            root[i] = root[i - d];
            for c in 0..6 {
                acc[i][c] = acc[i - d][c] + dl[c];
            }
        }
    }
    (root, acc)
}

/// Palette rank of each selector along c0 -> c1 in four-colour mode.
const RANK: [u32; 4] = [0, 3, 1, 2];

/// Sum of the palette ranks of the four selectors of a quadrant code (0..=12).
pub fn rank_sum(code: u8) -> u32 {
    (0..4).map(|p| RANK[((code >> (2 * p)) & 3) as usize]).sum()
}

fn expand(v: i32, m: i32) -> f64 {
    (if m == 32 {
        (v << 3) | (v >> 2)
    } else {
        (v << 2) | (v >> 4)
    }) as f64
}

fn four_colour(e: &[i32; 6]) -> bool {
    ((e[0] << 11) | (e[1] << 5) | e[2]) > ((e[3] << 11) | (e[4] << 5) | e[5])
}

/// Exact learning from (chunk lanes, true blocks) pairs.
#[derive(Default)]
pub struct ExactLearner {
    code_votes: Vec<[u32; 256]>,
    history_votes: Vec<HashMap<[u8; 6], u32>>,
}

impl ExactLearner {
    pub fn new() -> Self {
        ExactLearner {
            code_votes: vec![[0; 256]; 256],
            history_votes: vec![HashMap::new(); HISTORY_LEN],
        }
    }

    /// Adds one sample: decoded lanes and the `8 * n` bytes they must rebuild.
    pub fn add(&mut self, lanes: &[Vec<u8>], blocks: &[u8]) -> Result<()> {
        let nb = lanes[0].len();
        if blocks.len() != 8 * nb {
            return Err(Error::InvalidParams(format!(
                "reference has {} bytes, expected {}",
                blocks.len(),
                8 * nb
            )));
        }
        for i in 0..nb {
            let b = &blocks[8 * i..8 * i + 8];
            for (k, &code) in block_quadrants(b).iter().enumerate() {
                self.code_votes[lanes[8 + k][i] as usize][code as usize] += 1;
            }
            let d = unfold_distance(lanes[0][i]);
            if d > i {
                let e = block_endpoints(b);
                let mut entry = [0u8; 6];
                for c in 0..6 {
                    let dl = crate::bc1::zigzag_decode(lanes[2 + c][i]);
                    entry[c] = (e[c] as i32 - dl).rem_euclid(ENDPOINT_MOD[c]) as u8;
                }
                *self.history_votes[d - i - 1].entry(entry).or_insert(0) += 1;
            }
        }
        Ok(())
    }

    /// Majority vote; unobserved entries are taken from `initial`.
    pub fn finish(&self, initial: &Bc1Tables, report: &mut Report) -> Bc1Tables {
        let mut t = initial.clone();
        for (k, votes) in self.history_votes.iter().enumerate() {
            if let Some((e, _)) = votes
                .iter()
                .max_by_key(|(e, n)| (**n, std::cmp::Reverse(**e)))
            {
                t.history[k] = Some(*e);
                report.history_learned += 1;
                if votes.len() > 1 {
                    report.history_conflicts += 1;
                }
            }
        }
        // Quadrant codes: greedy assignment by vote count, keeping a permutation.
        let mut pairs: Vec<(u32, usize, usize)> = Vec::new();
        for (s, v) in self.code_votes.iter().enumerate() {
            let observed: Vec<usize> = (0..256).filter(|&c| v[c] > 0).collect();
            if !observed.is_empty() {
                report.symbols_observed += 1;
                if observed.len() > 1 {
                    report.symbol_conflicts += 1;
                }
            }
            for c in observed {
                pairs.push((v[c], s, c));
            }
        }
        pairs.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));
        let mut sym_done = [false; 256];
        let mut code_used = [false; 256];
        let mut codes = [0u8; 256];
        for (_, s, c) in pairs {
            if !sym_done[s] && !code_used[c] {
                sym_done[s] = true;
                code_used[c] = true;
                codes[s] = c as u8;
            }
        }
        fill_unassigned(
            &mut codes,
            &mut sym_done,
            &mut code_used,
            &initial.quadrant_codes,
        );
        t.quadrant_codes = codes;
        t
    }
}

/// Completes a partial symbol -> code assignment: unassigned symbols keep
/// their code from `fallback` when it is still free, then take the remaining
/// codes in increasing order.
fn fill_unassigned(
    codes: &mut [u8; 256],
    sym_done: &mut [bool; 256],
    code_used: &mut [bool; 256],
    fallback: &[u8; 256],
) {
    for s in 0..256 {
        let c = fallback[s] as usize;
        if !sym_done[s] && !code_used[c] {
            codes[s] = c as u8;
            sym_done[s] = true;
            code_used[c] = true;
        }
    }
    let free: Vec<usize> = (0..256).filter(|&c| !code_used[c]).collect();
    let mut free = free.into_iter();
    for s in 0..256 {
        if !sym_done[s] {
            let c = free.next().expect("as many free codes as free symbols");
            codes[s] = c as u8;
            code_used[c] = true;
            sym_done[s] = true;
        }
    }
}

/// Least-squares statistics of one (entry, channel, key) group.
#[derive(Default, Clone, Copy)]
struct Moments {
    aa: f64,
    bb: f64,
    ab: f64,
    ar: f64,
    br: f64,
}

/// Least-squares history fit from quadrant colour references.
pub struct QuadFitter {
    codes: [u8; 256],
    /// Per entry, per channel: (acc0 mod M, acc1 mod M) -> moments.
    groups: Vec<[HashMap<(u8, u8), Moments>; 3]>,
    blocks: Vec<usize>,
}

impl QuadFitter {
    pub fn new(codes: [u8; 256]) -> Self {
        QuadFitter {
            codes,
            groups: (0..HISTORY_LEN)
                .map(|_| [HashMap::new(), HashMap::new(), HashMap::new()])
                .collect(),
            blocks: vec![0; HISTORY_LEN],
        }
    }

    /// Adds one sample: decoded lanes and `12 * n` bytes of quadrant colours.
    pub fn add(&mut self, lanes: &[Vec<u8>], quads: &[u8]) -> Result<()> {
        let nb = lanes[0].len();
        if quads.len() != 12 * nb {
            return Err(Error::InvalidParams(format!(
                "quadrant reference has {} bytes, expected {}",
                quads.len(),
                12 * nb
            )));
        }
        let (root, acc) = chains(lanes);
        for i in 0..nb {
            let s = root[i];
            if s == 0 {
                continue;
            }
            self.blocks[s - 1] += 1;
            for c in 0..3 {
                let m = ENDPOINT_MOD[c];
                let key = (
                    acc[i][c].rem_euclid(m) as u8,
                    acc[i][3 + c].rem_euclid(m) as u8,
                );
                let g = self.groups[s - 1][c].entry(key).or_default();
                for qd in 0..4 {
                    let w = rank_sum(self.codes[lanes[8 + qd][i] as usize]) as f64 / 12.0;
                    let r = quads[12 * i + 3 * qd + c] as f64;
                    g.aa += (1.0 - w) * (1.0 - w);
                    g.bb += w * w;
                    g.ab += w * (1.0 - w);
                    g.ar += (1.0 - w) * r;
                    g.br += w * r;
                }
            }
        }
        Ok(())
    }

    /// Best entry for every history slot that roots at least one chain.
    pub fn finish(&self, initial: &Bc1Tables, report: &mut Report) -> Bc1Tables {
        let mut t = initial.clone();
        for k in 0..HISTORY_LEN {
            if self.blocks[k] == 0 {
                continue;
            }
            let mut entry = [0u8; 6];
            for c in 0..3 {
                let m = ENDPOINT_MOD[c];
                let ex: Vec<f64> = (0..m).map(|v| expand(v, m)).collect();
                let mut best = (f64::INFINITY, 0, 0);
                for a in 0..m {
                    for b in 0..m {
                        let mut err = 0.0;
                        for (&(k0, k1), g) in &self.groups[k][c] {
                            let e0 = ex[((a + k0 as i32) % m) as usize];
                            let e1 = ex[((b + k1 as i32) % m) as usize];
                            err += e0 * e0 * g.aa + e1 * e1 * g.bb + 2.0 * e0 * e1 * g.ab
                                - 2.0 * e0 * g.ar
                                - 2.0 * e1 * g.br;
                        }
                        if err < best.0 {
                            best = (err, a, b);
                        }
                    }
                }
                entry[c] = best.1 as u8;
                entry[3 + c] = best.2 as u8;
            }
            t.history[k] = Some(entry);
            report.history_learned += 1;
        }
        t
    }
}

/// Statistics for estimating the quadrant mapping without exact pairs.
pub struct PermEstimator {
    sym_freq: [f64; 256],
    sym_h: Vec<[f64; 256]>,
    sym_v: Vec<[f64; 256]>,
    w_num: [f64; 256],
    w_den: [f64; 256],
    w_n: [u32; 256],
    code_freq: [f64; 256],
    code_h: Vec<[f64; 256]>,
    code_v: Vec<[f64; 256]>,
}

impl Default for PermEstimator {
    fn default() -> Self {
        Self::new()
    }
}

impl PermEstimator {
    pub fn new() -> Self {
        PermEstimator {
            sym_freq: [0.0; 256],
            sym_h: vec![[0.0; 256]; 256],
            sym_v: vec![[0.0; 256]; 256],
            w_num: [0.0; 256],
            w_den: [0.0; 256],
            w_n: [0; 256],
            code_freq: [0.0; 256],
            code_h: vec![[0.0; 256]; 256],
            code_v: vec![[0.0; 256]; 256],
        }
    }

    /// Adds plain BC1 blocks to the corpus (four-colour blocks only).
    pub fn add_corpus(&mut self, blocks: &[u8]) {
        for b in blocks.chunks_exact(8) {
            let e = block_endpoints(b).map(|v| v as i32);
            if !four_colour(&e) {
                continue;
            }
            let q = block_quadrants(b).map(|c| c as usize);
            for &c in &q {
                self.code_freq[c] += 1.0;
            }
            self.code_h[q[0]][q[1]] += 1.0;
            self.code_h[q[2]][q[3]] += 1.0;
            self.code_v[q[0]][q[2]] += 1.0;
            self.code_v[q[1]][q[3]] += 1.0;
        }
    }

    /// Adds a sample: only blocks whose chain needs no history entry and that
    /// are in four-colour mode are used.
    pub fn add_sample(&mut self, lanes: &[Vec<u8>], quads: &[u8]) -> Result<()> {
        let nb = lanes[0].len();
        if quads.len() != 12 * nb {
            return Err(Error::InvalidParams(
                "quadrant reference size mismatch".into(),
            ));
        }
        let (root, acc) = chains(lanes);
        for i in 0..nb {
            if root[i] != 0 {
                continue;
            }
            let mut e = [0i32; 6];
            for c in 0..6 {
                e[c] = acc[i][c].rem_euclid(ENDPOINT_MOD[c]);
            }
            if !four_colour(&e) {
                continue;
            }
            let s: [usize; 4] = [0, 1, 2, 3].map(|k| lanes[8 + k][i] as usize);
            for &x in &s {
                self.sym_freq[x] += 1.0;
            }
            self.sym_h[s[0]][s[1]] += 1.0;
            self.sym_h[s[2]][s[3]] += 1.0;
            self.sym_v[s[0]][s[2]] += 1.0;
            self.sym_v[s[1]][s[3]] += 1.0;
            let c0: Vec<f64> = (0..3).map(|c| expand(e[c], ENDPOINT_MOD[c])).collect();
            let c1: Vec<f64> = (0..3).map(|c| expand(e[3 + c], ENDPOINT_MOD[c])).collect();
            let den: f64 = (0..3).map(|c| (c1[c] - c0[c]).powi(2)).sum();
            if den < 300.0 {
                continue; // endpoints too close: the weight is not measurable
            }
            for (qd, &x) in s.iter().enumerate() {
                let num: f64 = (0..3)
                    .map(|c| (quads[12 * i + 3 * qd + c] as f64 - c0[c]) * (c1[c] - c0[c]))
                    .sum();
                self.w_num[x] += num;
                self.w_den[x] += den;
                self.w_n[x] += 1;
            }
        }
        Ok(())
    }

    /// Estimated symbol -> code mapping.
    pub fn estimate(&self, report: &mut Report) -> [u8; 256] {
        let target: Vec<Option<f64>> = (0..256)
            .map(|s| (self.w_n[s] >= 20).then(|| 12.0 * self.w_num[s] / self.w_den[s]))
            .collect();
        let mut syms: Vec<usize> = (0..256).collect();
        syms.sort_by(|&a, &b| {
            self.sym_freq[b]
                .total_cmp(&self.sym_freq[a])
                .then(a.cmp(&b))
        });
        let mut codes_by_freq: Vec<usize> = (0..256).collect();
        codes_by_freq.sort_by(|&a, &b| {
            self.code_freq[b]
                .total_cmp(&self.code_freq[a])
                .then(a.cmp(&b))
        });
        let mut used = [false; 256];
        let mut codes = [0u8; 256];
        for &s in &syms {
            let pick = codes_by_freq
                .iter()
                .copied()
                .filter(|&c| !used[c])
                .min_by_key(|&c| match target[s] {
                    Some(t) => (rank_sum(c as u8) as i64 - t.round() as i64).unsigned_abs(),
                    None => 0,
                })
                .expect("a free code remains");
            used[pick] = true;
            codes[s] = pick as u8;
        }
        // Bigram refinement.
        let logp = |m: &Vec<[f64; 256]>| -> Vec<[f64; 256]> {
            let total: f64 = m.iter().map(|r| r.iter().sum::<f64>()).sum();
            let alpha = 0.5;
            let z = (total + alpha * 65536.0).ln();
            m.iter().map(|r| r.map(|v| (v + alpha).ln() - z)).collect()
        };
        let lh = logp(&self.code_h);
        let lv = logp(&self.code_v);
        let local = |codes: &[u8; 256], a: usize, b: usize| -> f64 {
            let mut ll = 0.0;
            for &x in &[a, b] {
                for t in 0..256 {
                    let (cx, ct) = (codes[x] as usize, codes[t] as usize);
                    ll += self.sym_h[x][t] * lh[cx][ct] + self.sym_v[x][t] * lv[cx][ct];
                    if t != a && t != b {
                        ll += self.sym_h[t][x] * lh[ct][cx] + self.sym_v[t][x] * lv[ct][cx];
                    }
                }
            }
            ll
        };
        let mut swaps = 0;
        for _pass in 0..50 {
            let mut changed = false;
            for a in 0..256 {
                for b in a + 1..256 {
                    let same_weight = rank_sum(codes[a]) == rank_sum(codes[b]);
                    if !same_weight && target[a].is_some() && target[b].is_some() {
                        continue;
                    }
                    let before = local(&codes, a, b);
                    codes.swap(a, b);
                    if local(&codes, a, b) > before + 1e-9 {
                        swaps += 1;
                        changed = true;
                    } else {
                        codes.swap(a, b);
                    }
                }
            }
            if !changed {
                break;
            }
        }
        let measured = target.iter().filter(|t| t.is_some()).count();
        report.notes.push(format!(
            "quadrant mapping estimated statistically ({measured} symbols with a measured weight, {swaps} bigram swaps)"
        ));
        report.symbols_observed = (0..256).filter(|&s| self.sym_freq[s] > 0.0).count();
        codes
    }
}

/// Runs the whole `learn-tables` command.
pub fn learn_from_files(opts: &LearnOptions) -> Result<(Bc1Tables, Report)> {
    let specs = read_list(&opts.list)?;
    let initial = match &opts.initial {
        Some(p) => Bc1Tables::load(p)?,
        None => Bc1Tables::default(),
    };
    let params = CodecParams::bc1();
    let mut report = Report::default();
    // Decode every sample once; keep only what each pass needs.
    let mut decoded: Vec<(usize, Vec<Vec<u8>>, Vec<u8>)> = Vec::new();
    let load = |s: &SampleSpec| -> Result<(Vec<Vec<u8>>, Vec<u8>)> {
        let lanes = decode_lanes(&read_file(&s.chunk)?, &params, s.size)?;
        Ok((lanes, read_file(&s.reference)?))
    };
    match opts.kind {
        RefKind::Blocks => {
            let mut l = ExactLearner::new();
            for (n, s) in specs.iter().enumerate() {
                match load(s).and_then(|(lanes, r)| l.add(&lanes, &r).map(|_| (lanes, r))) {
                    Ok((lanes, r)) => {
                        report.samples += 1;
                        decoded.push((n, lanes, r));
                    }
                    Err(e) => report.skipped.push(format!("{}: {e}", s.chunk.display())),
                }
            }
            let t = l.finish(&initial, &mut report);
            let ok = decoded
                .iter()
                .filter(|(_, lanes, r)| {
                    crate::bc1::assemble_bc1(lanes, &t).as_deref() == Ok(&r[..])
                })
                .count();
            report.self_check = Some((ok, decoded.len()));
            Ok((t, report))
        }
        RefKind::Quads => {
            let mut usable = Vec::new();
            for s in &specs {
                match load(s) {
                    Ok((lanes, r)) if r.len() == 12 * lanes[0].len() => usable.push(s.clone()),
                    Ok(_) => report.skipped.push(format!(
                        "{}: quadrant reference size mismatch",
                        s.chunk.display()
                    )),
                    Err(e) => report.skipped.push(format!("{}: {e}", s.chunk.display())),
                }
            }
            report.samples = usable.len();
            let codes = if opts.initial.is_some() {
                initial.quadrant_codes
            } else {
                let corpus = opts.corpus.as_ref().ok_or_else(|| {
                    Error::InvalidParams(
                        "--ref quads needs either --tables (quadrant mapping) or --corpus (plain BC1 blocks)".into(),
                    )
                })?;
                let mut pe = PermEstimator::new();
                let text = std::fs::read_to_string(corpus)
                    .map_err(|e| Error::Io(format!("{}: {e}", corpus.display())))?;
                let base = corpus.parent().unwrap_or(Path::new("."));
                for line in text
                    .lines()
                    .map(str::trim)
                    .filter(|l| !l.is_empty() && !l.starts_with('#'))
                {
                    pe.add_corpus(&read_file(&base.join(line))?);
                }
                for s in &usable {
                    let (lanes, r) = load(s)?;
                    pe.add_sample(&lanes, &r)?;
                }
                pe.estimate(&mut report)
            };
            let mut fit = QuadFitter::new(codes);
            for s in &usable {
                let (lanes, r) = load(s)?;
                fit.add(&lanes, &r)?;
            }
            let mut t = fit.finish(&initial, &mut report);
            t.quadrant_codes = codes;
            report.notes.push(
                "history entries fitted by least squares: R and B are usually exact, G can be off by one".into(),
            );
            Ok((t, report))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bc1::{assemble_bc1, lanes_for_blocks};
    use crate::testenc::Rng;

    fn random_tables(rng: &mut Rng) -> Bc1Tables {
        let mut t = Bc1Tables::default();
        for k in (1..256).rev() {
            let j = rng.below(k as u64 + 1) as usize;
            t.quadrant_codes.swap(k, j);
        }
        for e in t.history.iter_mut() {
            *e = Some([0, 1, 2, 3, 4, 5].map(|c| rng.below(ENDPOINT_MOD[c] as u64) as u8));
        }
        t
    }

    #[test]
    fn exact_learning_recovers_tables() {
        let mut rng = Rng::new(11);
        let truth = random_tables(&mut rng);
        let mut l = ExactLearner::new();
        let mut samples = Vec::new();
        for _ in 0..40 {
            let nb = 300;
            let mut blocks = vec![0u8; 8 * nb];
            rng.fill(&mut blocks);
            let d: Vec<usize> = (0..nb).map(|_| rng.below(256) as usize).collect();
            let lanes = lanes_for_blocks(&blocks, &d, &truth).unwrap();
            l.add(&lanes, &blocks).unwrap();
            samples.push((lanes, blocks));
        }
        let mut rep = Report::default();
        let t = l.finish(&Bc1Tables::default(), &mut rep);
        assert_eq!(rep.symbols_observed, 256);
        assert_eq!(rep.symbol_conflicts, 0);
        assert_eq!(rep.history_conflicts, 0);
        assert_eq!(t.quadrant_codes, truth.quadrant_codes);
        for (k, e) in t.history.iter().enumerate() {
            if e.is_some() {
                assert_eq!(*e, truth.history[k]);
            }
        }
        for (lanes, blocks) in &samples {
            assert_eq!(&assemble_bc1(lanes, &t).unwrap(), blocks);
        }
    }

    /// Renders BC1 blocks to quadrant colours (2 x 2 average of the pixels).
    fn quads_of(blocks: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        for b in blocks.chunks_exact(8) {
            let e = block_endpoints(b).map(|v| v as i32);
            let c0: Vec<f64> = (0..3).map(|c| expand(e[c], ENDPOINT_MOD[c])).collect();
            let c1: Vec<f64> = (0..3).map(|c| expand(e[3 + c], ENDPOINT_MOD[c])).collect();
            let pal = |sel: u8, c: usize| match sel {
                0 => c0[c],
                1 => c1[c],
                2 => (2.0 * c0[c] + c1[c]) / 3.0,
                _ => (c0[c] + 2.0 * c1[c]) / 3.0,
            };
            for code in block_quadrants(b) {
                for c in 0..3 {
                    let s: f64 = (0..4).map(|p| pal((code >> (2 * p)) & 3, c)).sum();
                    out.push((s / 4.0).round() as u8);
                }
            }
        }
        out
    }

    #[test]
    fn quad_fit_recovers_history() {
        let mut rng = Rng::new(12);
        let truth = random_tables(&mut rng);
        let mut fit = QuadFitter::new(truth.quadrant_codes);
        for _ in 0..30 {
            let nb = 256;
            // Smooth four-colour blocks: c0 > c1 with a clear gap.
            let mut blocks = vec![0u8; 8 * nb];
            for b in blocks.chunks_exact_mut(8) {
                let hi = [16 + rng.below(16), 32 + rng.below(32), 16 + rng.below(16)];
                let lo = [rng.below(12), rng.below(24), rng.below(12)];
                let c0 = (hi[0] << 11 | hi[1] << 5 | hi[2]) as u16;
                let c1 = (lo[0] << 11 | lo[1] << 5 | lo[2]) as u16;
                b[..2].copy_from_slice(&c0.to_le_bytes());
                b[2..4].copy_from_slice(&c1.to_le_bytes());
                rng.fill(&mut b[4..]);
            }
            let d: Vec<usize> = (0..nb)
                .map(|i| {
                    if i < 20 {
                        21 + rng.below(30) as usize
                    } else {
                        0
                    }
                })
                .collect();
            let lanes = lanes_for_blocks(&blocks, &d, &truth).unwrap();
            fit.add(&lanes, &quads_of(&blocks)).unwrap();
        }
        let mut rep = Report::default();
        let t = fit.finish(&Bc1Tables::default(), &mut rep);
        let mut checked = 0;
        for k in 0..HISTORY_LEN {
            if let Some(e) = t.history[k] {
                let want = truth.history[k].unwrap();
                for c in 0..6 {
                    assert!(
                        (e[c] as i32 - want[c] as i32).abs() <= 1,
                        "slot {} ch {c}: {e:?} vs {want:?}",
                        k + 1
                    );
                }
                checked += 1;
            }
        }
        assert!(checked >= 20);
    }
}
