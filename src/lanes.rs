//! Entropy layer: interleaved rANS lanes and the byte scheduler.
//!
//! A BCPack chunk is a single byte stream shared by `K` lanes (12 for BC1,
//! BC3, BC4 and BC5; 16 for BC6H and BC7). Each lane decodes one symbol per
//! coding unit (8 output bytes: one BC1/BC4 block, half a BC3/BC5/BC6H/BC7
//! block) and is
//! made of a chain of *records* (fragments). Bytes are not stored lane by
//! lane: the decoder works in rounds, and in each round at most
//! `max_reads_per_round` (8) lanes that want a byte receive the next byte of
//! the stream, in lane-number order. See `docs/SPEC.md` for the full rules.

use crate::error::{Error, Result};
use crate::model::{models, ModelTable, Models, L, MAX_WIDTH};

/// Layout of one codec: number of lanes, their base symbol widths, and the
/// coding unit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodecParams {
    /// Base symbol width of each lane, in bits (1..=8). A record may reduce
    /// the width by 0..=3 bits.
    pub lane_widths: Vec<u32>,
    /// Output bytes covered by one symbol of every lane (8 for BC1).
    pub unit_bytes: usize,
    /// Maximum number of bytes handed out per scheduler round (8).
    pub max_reads_per_round: usize,
}

impl CodecParams {
    /// BC1: 12 lanes, one symbol per lane and per 8-byte block.
    ///
    /// | lane | width | content                                  |
    /// |------|-------|------------------------------------------|
    /// | 0    | 8     | folded back-reference distance           |
    /// | 1    | 8     | unknown, always 0 in observed data       |
    /// | 2-4  | 5,6,5 | zigzag delta of c0 R, G, B               |
    /// | 5-7  | 5,6,5 | zigzag delta of c1 R, G, B               |
    /// | 8-11 | 8     | selector quadrants TL, TR, BL, BR        |
    pub fn bc1() -> Self {
        CodecParams {
            lane_widths: vec![8, 8, 5, 6, 5, 5, 6, 5, 8, 8, 8, 8],
            unit_bytes: 8,
            max_reads_per_round: 8,
        }
    }

    /// BC3: 12 lanes, two coding units (symbol tuples) per 16-byte block.
    /// Base widths `4,4,8,6,6,6,6,5,6,5,8,8`. Entropy layer only: the block
    /// semantics are not implemented by this crate.
    pub fn bc3() -> Self {
        Self::with_widths(vec![4, 4, 8, 6, 6, 6, 6, 5, 6, 5, 8, 8])
    }

    /// BC4: 12 lanes, one coding unit per 8-byte block. Base widths
    /// `8,8,8,8` then eight lanes of width 6. Entropy layer only.
    pub fn bc4() -> Self {
        Self::with_widths(vec![8, 8, 8, 8, 6, 6, 6, 6, 6, 6, 6, 6])
    }

    /// BC5: 12 lanes, two coding units per 16-byte block. Base widths
    /// `4,4,8,8` then eight lanes of width 6. Entropy layer only.
    pub fn bc5() -> Self {
        Self::with_widths(vec![4, 4, 8, 8, 6, 6, 6, 6, 6, 6, 6, 6])
    }

    /// BC6H: 16 lanes, two coding units per 16-byte block. Base widths
    /// `4,4,8,8,8,5,5,5,6,6,6,8,8,8,8,5`. Entropy layer only.
    pub fn bc6h() -> Self {
        Self::with_widths(vec![4, 4, 8, 8, 8, 5, 5, 5, 6, 6, 6, 8, 8, 8, 8, 5])
    }

    /// BC7: 16 lanes, two coding units per 16-byte block. Base widths
    /// `4,4,8,8,8,8,8,7,7,5,6,8,8,8,8,5` (lane 15 is 5 bits wide). Entropy
    /// layer only.
    pub fn bc7() -> Self {
        Self::with_widths(vec![4, 4, 8, 8, 8, 8, 8, 7, 7, 5, 6, 8, 8, 8, 8, 5])
    }

    /// Lane layout for a codec name (`bc1`, `bc3`, `bc4`, `bc5`, `bc6h`,
    /// `bc7`), case-insensitive.
    pub fn for_codec(name: &str) -> Option<Self> {
        match name.to_ascii_lowercase().as_str() {
            "bc1" => Some(Self::bc1()),
            "bc3" => Some(Self::bc3()),
            "bc4" => Some(Self::bc4()),
            "bc5" => Some(Self::bc5()),
            "bc6h" => Some(Self::bc6h()),
            "bc7" => Some(Self::bc7()),
            _ => None,
        }
    }

    /// Generic parameters with the BCPack defaults (8-byte unit, 8 reads per round).
    pub fn with_widths(lane_widths: Vec<u32>) -> Self {
        CodecParams {
            lane_widths,
            unit_bytes: 8,
            max_reads_per_round: 8,
        }
    }

    fn validate(&self) -> Result<()> {
        if self.lane_widths.is_empty() {
            return Err(Error::InvalidParams("no lanes".into()));
        }
        if let Some(w) = self.lane_widths.iter().find(|&&w| w == 0 || w > MAX_WIDTH) {
            return Err(Error::InvalidParams(format!(
                "lane width {w} outside 1..=8"
            )));
        }
        if self.unit_bytes == 0 {
            return Err(Error::InvalidParams("unit_bytes must be at least 1".into()));
        }
        if self.max_reads_per_round == 0 {
            return Err(Error::InvalidParams(
                "max_reads_per_round must be at least 1".into(),
            ));
        }
        Ok(())
    }
}

/// Where the scheduler takes its bytes from.
///
/// The decoder uses a single shared stream; the test encoder uses one queue
/// per lane and records the order in which bytes are requested, which yields
/// exactly the interleaved stream a decoder would read.
pub trait ByteSource {
    /// Next byte for `lane`, or `None` when the source is exhausted.
    fn next_byte(&mut self, lane: usize) -> Option<u8>;
    /// Bytes handed out so far (for error messages).
    fn position(&self) -> usize;
}

/// A plain shared byte stream.
pub struct SharedStream<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> SharedStream<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        SharedStream { data, pos: 0 }
    }
}

impl ByteSource for SharedStream<'_> {
    fn next_byte(&mut self, _lane: usize) -> Option<u8> {
        let b = *self.data.get(self.pos)?;
        self.pos += 1;
        Some(b)
    }
    fn position(&self) -> usize {
        self.pos
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// Reading a record header (mode, length, state bytes).
    Header,
    /// Decoding the symbols of a record.
    Run,
    /// All symbols decoded.
    Done,
}

/// Parsed record header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecordHeader {
    /// Model index (mode bits 0-3).
    pub q: u32,
    /// Flush flag (mode bit 4): the record ends with the state back at `L`
    /// and the next record of the lane starts from a fresh state. When clear,
    /// the record ends as soon as its bytes are exhausted and the state is
    /// below `L`, and the next record continues from that state.
    pub flush: bool,
    /// Width reduction (mode bits 6-7).
    pub reduction: u32,
    /// Total record bytes, state included.
    pub total_bytes: u64,
    /// Number of state bytes stored (1..=4; trailing zero bytes are trimmed),
    /// or 0 when the record continues the state of a non-flushed record.
    pub state_bytes: usize,
}

/// Parses a header prefix. Returns `None` while more bytes are needed.
///
/// Layout: `mode len_lo [len_hi if mode bit 5] state[0..min(4, total)]`.
/// * mode bits 0-3: `q`; bit 4: flush; bit 5: extended length; bits 6-7:
///   width reduction `r` (`w' = w - r`).
/// * short length: `total = len + 1` (1..=256);
///   extended: `total = (lo | hi << 8) + 257`.
/// * `with_state`: the record starts from a fresh state (first record of the
///   lane, or the previous record had its flush bit set), so its first
///   `min(4, total)` bytes are the initial state. Otherwise the record has no
///   state bytes and continues the state left by the previous record.
pub fn parse_header(hdr: &[u8], with_state: bool) -> Option<RecordHeader> {
    let m = *hdr.first()? as u32;
    let ext = ((m >> 5) & 1) as usize;
    let nlen = 1 + ext;
    if hdr.len() < 1 + nlen {
        return None;
    }
    let ln: u64 = if ext == 1 {
        (hdr[1] as u64 | (hdr[2] as u64) << 8) + 256
    } else {
        hdr[1] as u64
    };
    let total = ln + 1;
    let nst = if with_state { total.min(4) as usize } else { 0 };
    if hdr.len() < 1 + nlen + nst {
        return None;
    }
    Some(RecordHeader {
        q: m & 15,
        flush: m & 0x10 != 0,
        reduction: m >> 6,
        total_bytes: total,
        state_bytes: nst,
    })
}

struct Lane<'m> {
    index: usize,
    width: u32,
    phase: Phase,
    hdr: [u8; 7],
    hdr_len: usize,
    /// One byte fetched but not yet merged into the state.
    buf: Option<u8>,
    /// Record bytes not yet fetched.
    budget: u64,
    x: u64,
    table: Option<&'m ModelTable>,
    /// Flush bit of the current record.
    flush: bool,
    /// The previous record was not flushed: the next header has no state
    /// bytes and the state `x` carries over.
    carry: bool,
    /// Symbols decoded ahead of the block synchroniser (0 or 1).
    pending: u32,
    /// Header completed in this round: no decoding until the next round.
    fresh: bool,
    syms: Vec<u8>,
}

impl<'m> Lane<'m> {
    /// Feeds one header byte; returns `true` when the header is complete.
    fn feed_header(&mut self, byte: u8, models: &'m Models) -> Result<bool> {
        self.hdr[self.hdr_len] = byte;
        self.hdr_len += 1;
        let h = match parse_header(&self.hdr[..self.hdr_len], !self.carry) {
            None => return Ok(false),
            Some(h) => h,
        };
        let w = self.width as i32 - h.reduction as i32;
        if w < 1 || w > MAX_WIDTH as i32 {
            return Err(Error::WidthOutOfRange {
                lane: self.index,
                width: w,
            });
        }
        if !self.carry {
            let start = self.hdr_len - h.state_bytes;
            self.x = self.hdr[start..self.hdr_len]
                .iter()
                .fold(0u64, |x, &b| (x << 8) | b as u64);
        }
        self.carry = false;
        self.flush = h.flush;
        self.budget = h.total_bytes - h.state_bytes as u64;
        self.table = Some(models.get(h.q, w as u32));
        self.phase = Phase::Run;
        Ok(true)
    }

    /// True when the record is exhausted and the state is back at `L`,
    /// or exhausted and pointing at the FIN slot.
    fn at_end(&self) -> bool {
        self.budget == 0
            && self.buf.is_none()
            && self.x >= L
            && (self.x == L || self.x & 0xffff == 0)
    }

    /// Closes the current record. Only `x == L` is a valid end: decoding the
    /// FIN slot from any other state cannot renormalise back to `L`.
    fn finish(&mut self, nsym: usize) -> Result<()> {
        if self.x != L {
            return Err(Error::BadFragmentEnd {
                lane: self.index,
                state: self.x,
            });
        }
        if self.syms.len() >= nsym {
            self.phase = Phase::Done;
        } else {
            self.phase = Phase::Header;
            self.hdr_len = 0;
            self.carry = !self.flush;
        }
        Ok(())
    }

    /// Closes a non-flushed record whose bytes are exhausted while the state
    /// is below `L`: the next record continues from the current state.
    fn finish_carry(&mut self, nsym: usize) {
        if self.syms.len() >= nsym {
            self.phase = Phase::Done;
        } else {
            self.phase = Phase::Header;
            self.hdr_len = 0;
            self.carry = true;
        }
    }

    /// Record exhausted with the state below `L`: either shift in the trimmed
    /// zero bytes (flushed record) or hand over to the next record (returns
    /// `true`).
    fn exhausted(&mut self, nsym: usize) -> Result<bool> {
        if self.flush {
            self.zero_fill()?;
            Ok(false)
        } else {
            self.finish_carry(nsym);
            Ok(true)
        }
    }

    fn zero_fill(&mut self) -> Result<()> {
        if self.x == 0 {
            return Err(Error::ZeroState { lane: self.index });
        }
        while self.x < L {
            self.x <<= 8;
        }
        Ok(())
    }

    fn wants_byte(&self) -> bool {
        match self.phase {
            Phase::Header => true,
            Phase::Run => {
                self.buf.is_none() && self.budget > 0 && (self.x < L || self.pending == 0)
            }
            Phase::Done => false,
        }
    }
}

/// Runs the scheduler over any byte source and returns the lane symbols.
///
/// `nsym` is the number of symbols every lane must produce.
pub fn run_scheduler<S: ByteSource>(
    src: &mut S,
    params: &CodecParams,
    nsym: usize,
) -> Result<Vec<Vec<u8>>> {
    params.validate()?;
    let models = models()?;
    let n = params.lane_widths.len();
    let mut lanes: Vec<Lane> = params
        .lane_widths
        .iter()
        .enumerate()
        .map(|(index, &width)| Lane {
            index,
            width,
            phase: if nsym == 0 {
                Phase::Done
            } else {
                Phase::Header
            },
            hdr: [0; 7],
            hdr_len: 0,
            buf: None,
            budget: 0,
            x: 0,
            table: None,
            flush: true,
            carry: false,
            pending: 0,
            fresh: false,
            syms: Vec::with_capacity(nsym),
        })
        .collect();

    let max_rounds = 20 * nsym + 200;
    let mut rounds = 0usize;
    let mut blocks = 0usize;
    let mut served = vec![false; n];
    let mut stalled = vec![false; n];

    while !lanes.iter().all(|l| l.phase == Phase::Done) {
        // 1. Requests: the first `max_reads_per_round` lanes (by number) that
        //    want a byte are served; a lane inside a record that wanted a byte
        //    and was not served is stalled for this round.
        let mut granted = 0;
        for (i, l) in lanes.iter().enumerate() {
            served[i] = false;
            stalled[i] = false;
            if l.wants_byte() {
                if granted < params.max_reads_per_round {
                    served[i] = true;
                    granted += 1;
                } else if l.phase == Phase::Run {
                    stalled[i] = true;
                }
            }
        }

        // 2. Reads, in lane order.
        for (i, l) in lanes.iter_mut().enumerate() {
            if !served[i] {
                continue;
            }
            let byte = src.next_byte(i).ok_or(Error::UnexpectedEof {
                lane: i,
                offset: src.position(),
            })?;
            match l.phase {
                Phase::Header => {
                    if l.feed_header(byte, models)? {
                        l.fresh = true;
                    }
                }
                Phase::Run => {
                    l.buf = Some(byte);
                    l.budget -= 1;
                }
                Phase::Done => unreachable!("done lanes never request bytes"),
            }
        }

        // 3. Decoding.
        for (i, l) in lanes.iter_mut().enumerate() {
            if l.phase != Phase::Run {
                continue;
            }
            if l.fresh {
                l.fresh = false;
                continue;
            }
            if l.x < L {
                match l.buf.take() {
                    // Merging a byte costs the round.
                    Some(b) => {
                        l.x = (l.x << 8) | b as u64;
                        continue;
                    }
                    None if l.budget == 0 => {
                        if l.exhausted(nsym)? {
                            continue;
                        }
                    }
                    None => continue,
                }
            }
            if l.at_end() {
                l.finish(nsym)?;
                continue;
            }
            if l.pending >= 1 {
                continue;
            }
            if l.syms.len() >= nsym {
                return Err(Error::ExtraSymbols { lane: i });
            }
            if stalled[i] {
                continue;
            }
            // Decode one symbol.
            let table = l.table.expect("table is set in Run phase");
            let slot = (l.x & 0xffff) as u32;
            let v = table.lookup(slot).ok_or(Error::FinBeforeEnd {
                lane: i,
                symbol: l.syms.len(),
            })?;
            l.x = table.freq(v) as u64 * (l.x >> 16) + (slot - table.cum[v]) as u64;
            l.syms.push(v as u8);
            l.pending += 1;
            if l.x < L {
                if let Some(b) = l.buf.take() {
                    l.x = (l.x << 8) | b as u64;
                }
            }
            // Trailing zero bytes of a record are not stored: once the record
            // is exhausted they are shifted in for free.
            if l.budget == 0 && l.buf.is_none() && l.x < L && l.exhausted(nsym)? {
                continue;
            }
            if l.at_end() {
                l.finish(nsym)?;
            }
        }

        // 4. Block synchronisation: a lane may run at most one symbol ahead.
        if lanes
            .iter()
            .all(|l| l.pending > 0 || (l.phase == Phase::Done && l.syms.len() > blocks))
        {
            for l in lanes.iter_mut() {
                l.pending = l.pending.saturating_sub(1);
            }
            blocks += 1;
        }

        rounds += 1;
        if rounds > max_rounds && !lanes.iter().all(|l| l.phase == Phase::Done) {
            return Err(Error::RoundLimit { rounds });
        }
    }

    for l in &lanes {
        if l.syms.len() != nsym || l.budget != 0 || l.buf.is_some() {
            return Err(Error::LaneIncomplete {
                lane: l.index,
                symbols: l.syms.len(),
                expected: nsym,
            });
        }
    }
    Ok(lanes.into_iter().map(|l| l.syms).collect())
}

/// Decodes the lanes of one chunk.
///
/// * `raw`: the BCPack chunk bytes;
/// * `params`: lane layout of the codec;
/// * `out_size`: size of the decoded data in bytes (a multiple of
///   `params.unit_bytes`).
///
/// Returns one symbol vector per lane, each `out_size / unit_bytes` long.
/// Fails unless every record ends exactly and every input byte is consumed.
pub fn decode_lanes(raw: &[u8], params: &CodecParams, out_size: usize) -> Result<Vec<Vec<u8>>> {
    params.validate()?;
    if out_size % params.unit_bytes != 0 {
        return Err(Error::OutSizeNotMultiple {
            out_size,
            unit: params.unit_bytes,
        });
    }
    let nsym = out_size / params.unit_bytes;
    let mut src = SharedStream::new(raw);
    let lanes = run_scheduler(&mut src, params, nsym)?;
    if src.position() != raw.len() {
        return Err(Error::TrailingBytes {
            consumed: src.position(),
            total: raw.len(),
        });
    }
    Ok(lanes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_parsing() {
        // short length: 0x05 + 1 = 6 bytes, 4 of them state
        let h = parse_header(&[0x12, 0x05, 1, 2, 3, 4], true).unwrap();
        assert_eq!(
            h,
            RecordHeader {
                q: 2,
                flush: true,
                reduction: 0,
                total_bytes: 6,
                state_bytes: 4
            }
        );
        assert!(parse_header(&[0x12, 0x05, 1, 2, 3], true).is_none());
        // extended length: (0x0b | 0x00 << 8) + 257 = 268
        let h = parse_header(&[0xbc, 0x0b, 0x00, 1, 2, 3, 4], true).unwrap();
        assert_eq!(
            h,
            RecordHeader {
                q: 12,
                flush: true,
                reduction: 2,
                total_bytes: 268,
                state_bytes: 4
            }
        );
        // 1-byte record: the state is a single byte
        let h = parse_header(&[0x50, 0x00, 0x01], true).unwrap();
        assert_eq!(h.total_bytes, 1);
        assert_eq!(h.state_bytes, 1);
        assert_eq!(h.reduction, 1);
    }

    #[test]
    fn header_without_state() {
        // flush bit clear does not change the layout of this header...
        let h = parse_header(&[0x02, 0x05, 1, 2, 3, 4], true).unwrap();
        assert!(!h.flush);
        assert_eq!(h.state_bytes, 4);
        // ...but a record that continues a non-flushed one has no state bytes
        let h = parse_header(&[0x12, 0x05], false).unwrap();
        assert!(h.flush);
        assert_eq!((h.total_bytes, h.state_bytes), (6, 0));
        let h = parse_header(&[0x22, 0x0b, 0x00], false).unwrap();
        assert_eq!((h.total_bytes, h.state_bytes), (268, 0));
    }

    #[test]
    fn codec_layouts() {
        for (name, n) in [
            ("bc1", 12),
            ("BC3", 12),
            ("bc4", 12),
            ("bc5", 12),
            ("bc6h", 16),
            ("bc7", 16),
        ] {
            let p = CodecParams::for_codec(name).unwrap();
            assert_eq!(p.lane_widths.len(), n);
            assert!(p.validate().is_ok());
        }
        assert_eq!(CodecParams::bc7().lane_widths[15], 5);
        assert!(CodecParams::for_codec("bc2").is_none());
    }

    #[test]
    fn empty_output_needs_empty_input() {
        let p = CodecParams::bc1();
        assert_eq!(
            decode_lanes(&[], &p, 0).unwrap(),
            vec![Vec::<u8>::new(); 12]
        );
        assert_eq!(
            decode_lanes(&[1], &p, 0),
            Err(Error::TrailingBytes {
                consumed: 0,
                total: 1
            })
        );
    }

    #[test]
    fn error_paths() {
        let p = CodecParams::bc1();
        assert!(matches!(
            decode_lanes(&[], &p, 7),
            Err(Error::OutSizeNotMultiple { .. })
        ));
        assert!(matches!(
            decode_lanes(&[], &p, 8),
            Err(Error::UnexpectedEof { lane: 0, .. })
        ));
        // zero bytes: non-flushed records that never yield a symbol
        assert!(decode_lanes(&[0x00; 64], &p, 8).is_err());
        assert!(matches!(
            decode_lanes(&[], &CodecParams::with_widths(vec![]), 8),
            Err(Error::InvalidParams(_))
        ));
        assert!(matches!(
            decode_lanes(&[], &CodecParams::with_widths(vec![9]), 8),
            Err(Error::InvalidParams(_))
        ));
        // width reduction larger than the lane width
        let p1 = CodecParams::with_widths(vec![2]);
        assert!(matches!(
            decode_lanes(&[0xd0, 0x00, 0x01], &p1, 8),
            Err(Error::WidthOutOfRange { lane: 0, width: -1 })
        ));
    }

    #[test]
    fn empty_record_then_end_of_input() {
        // One lane of width 3 that needs one symbol. The record `10 00 01`
        // (q = 0, 1 byte, state 0x01 = trimmed 0x01000000 = L) is valid but
        // holds no symbol, so the lane asks for another header and the input
        // is exhausted.
        let p = CodecParams::with_widths(vec![3]);
        assert!(matches!(
            decode_lanes(&[0x10, 0x00, 0x01], &p, 8),
            Err(Error::UnexpectedEof { lane: 0, .. })
        ));
    }
}
