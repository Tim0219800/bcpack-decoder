//! Test-only BCPack encoder.
//!
//! It produces streams the decoder must accept, so that round trips can be
//! tested without any real data:
//!
//! 1. each lane's symbols are split into records and every record is rANS
//!    encoded on its own (ryg-style encoder starting from `x = L`, state
//!    written big-endian, trailing zero bytes trimmed);
//! 2. the records are interleaved by running the decoder's own scheduler on
//!    per-lane byte queues and recording the order in which bytes are
//!    requested. Because the scheduler only ever reads a lane's bytes in
//!    order, that order is exactly the stream a decoder will consume.

use crate::error::Result;
use crate::lanes::{run_scheduler, ByteSource, CodecParams};
use crate::model::{models, ModelTable, L};
use std::collections::VecDeque;

/// Small deterministic PRNG (xorshift64*).
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Rng(seed.wrapping_mul(0x9E3779B97F4A7C15) | 1)
    }
    pub fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545F4914F6CDD1D)
    }
    pub fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
    pub fn fill(&mut self, b: &mut [u8]) {
        for x in b {
            *x = self.next() as u8;
        }
    }
    /// Symbol drawn from a model table (so records compress realistically).
    pub fn sample_from(&mut self, t: &ModelTable) -> u8 {
        let slot = 1 + self.below(65535) as u32;
        t.lookup(slot).unwrap() as u8
    }
}

/// One record of one lane.
#[derive(Debug, Clone)]
pub struct Record {
    pub q: u32,
    pub reduction: u32,
    pub syms: Vec<u8>,
}

/// rANS-encodes one record body (state + payload, trailing zeros trimmed).
pub fn encode_body(table: &ModelTable, syms: &[u8]) -> Vec<u8> {
    let mut x: u64 = L;
    let mut emitted = Vec::new();
    for &s in syms.iter().rev() {
        let f = table.freq(s as usize) as u64;
        let c = table.cum[s as usize] as u64;
        let x_max = ((L >> 16) << 8) * f;
        while x >= x_max {
            emitted.push(x as u8);
            x >>= 8;
        }
        x = ((x / f) << 16) + (x % f) + c;
    }
    let mut body = (x as u32).to_be_bytes().to_vec();
    body.extend(emitted.iter().rev());
    while body.len() > 1 && *body.last().unwrap() == 0 {
        body.pop();
    }
    body
}

/// Encodes one record: header + body.
pub fn encode_record(width: u32, r: &Record) -> Vec<u8> {
    let w = width - r.reduction;
    let table = models().unwrap().get(r.q, w);
    let body = encode_body(table, &r.syms);
    let total = body.len();
    let mode = ((r.reduction << 6) | 0x10 | r.q) as u8;
    let mut out = if total <= 256 {
        vec![mode, (total - 1) as u8]
    } else {
        let stored = total - 257;
        assert!(stored <= 0xffff, "record too long for the test encoder");
        vec![mode | 0x20, stored as u8, (stored >> 8) as u8]
    };
    out.extend(body);
    out
}

struct Queues {
    q: Vec<VecDeque<u8>>,
    out: Vec<u8>,
}

impl ByteSource for Queues {
    fn next_byte(&mut self, lane: usize) -> Option<u8> {
        let b = self.q[lane].pop_front()?;
        self.out.push(b);
        Some(b)
    }
    fn position(&self) -> usize {
        self.out.len()
    }
}

/// Interleaves pre-encoded per-lane byte strings (records already built by
/// the caller) and checks that every lane re-decodes to `want`.
pub fn interleave_raw(
    params: &CodecParams,
    lanes: Vec<Vec<u8>>,
    want: &[Vec<u8>],
    nsym: usize,
) -> Result<Vec<u8>> {
    let mut src = Queues {
        q: lanes.into_iter().map(VecDeque::from).collect(),
        out: Vec::new(),
    };
    let syms = run_scheduler(&mut src, params, nsym)?;
    for lane in 0..want.len() {
        assert_eq!(syms[lane], want[lane], "lane {lane} re-decoded differently");
        assert!(src.q[lane].is_empty(), "lane {lane} has unread bytes");
    }
    Ok(src.out)
}

/// Header bytes of a record of `total` bytes.
pub fn header(q: u32, reduction: u32, flush: bool, total: usize) -> Vec<u8> {
    let mode = ((reduction << 6) | if flush { 0x10 } else { 0 } | q) as u8;
    if total <= 256 {
        vec![mode, (total - 1) as u8]
    } else {
        let stored = total - 257;
        vec![mode | 0x20, stored as u8, (stored >> 8) as u8]
    }
}

/// Encodes `syms` as one rANS body, then cuts it after `cut` bytes into a
/// non-flushed record followed by a flushed record that continues its state.
/// Both records use the same model, so the pair must decode exactly like the
/// single record.
pub fn split_record(width: u32, q: u32, syms: &[u8], cut: usize) -> Vec<u8> {
    let table = models().unwrap().get(q, width);
    let body = encode_body(table, syms);
    assert!(cut >= 1 && cut < body.len());
    let mut out = header(q, 0, false, cut);
    out.extend(&body[..cut]);
    out.extend(header(q, 0, true, body.len() - cut));
    out.extend(&body[cut..]);
    out
}

/// Builds the interleaved stream for the given records of every lane.
/// Every lane's records must hold exactly `nsym` symbols in total.
pub fn interleave(params: &CodecParams, records: &[Vec<Record>], nsym: usize) -> Result<Vec<u8>> {
    let q = records
        .iter()
        .zip(&params.lane_widths)
        .map(|(recs, &w)| recs.iter().flat_map(|r| encode_record(w, r)).collect())
        .collect();
    let mut src = Queues { q, out: Vec::new() };
    let syms = run_scheduler(&mut src, params, nsym)?;
    for (lane, recs) in records.iter().enumerate() {
        let want: Vec<u8> = recs.iter().flat_map(|r| r.syms.iter().copied()).collect();
        assert_eq!(
            syms[lane], want,
            "scheduler re-decoded lane {lane} differently"
        );
        assert!(src.q[lane].is_empty(), "lane {lane} has unread bytes");
    }
    Ok(src.out)
}

/// Splits `syms` of a lane into random records, choosing models so that
/// every symbol fits the record width.
pub fn random_records(rng: &mut Rng, width: u32, syms: &[u8]) -> Vec<Record> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < syms.len() || out.is_empty() {
        let len = if rng.below(8) == 0 {
            0
        } else {
            1 + rng.below(40) as usize
        };
        let end = (i + len).min(syms.len());
        let part = &syms[i..end];
        let bits_needed = part
            .iter()
            .map(|&s| if s == 0 { 1 } else { 8 - s.leading_zeros() })
            .max()
            .unwrap_or(1);
        let max_red = (width - bits_needed.min(width)).min(3);
        let reduction = rng.below(max_red as u64 + 1) as u32;
        let q = rng.below(16) as u32;
        out.push(Record {
            q,
            reduction,
            syms: part.to_vec(),
        });
        i = end;
    }
    out
}

/// Random symbols for a lane, drawn from a random model of that width.
pub fn random_lane(rng: &mut Rng, width: u32, n: usize) -> Vec<u8> {
    let q = rng.below(16) as u32;
    let t = models().unwrap().get(q, width);
    (0..n).map(|_| rng.sample_from(t)).collect()
}
