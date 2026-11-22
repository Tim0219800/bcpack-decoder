//! Round-trip tests through the test-only encoder (no real data involved).

use crate::bc1::{decode_bc1, lanes_for_blocks};
use crate::error::Error;
use crate::lanes::{decode_lanes, CodecParams};
use crate::model::{models, ModelTable, L, M, MAX_Q, MAX_WIDTH};
use crate::tables::Bc1Tables;
use crate::testenc::*;

/// Sequential single-record decoder (no scheduling), used to check the
/// rANS primitive in isolation. Returns the symbols and the final state.
fn decode_record(
    table: &ModelTable,
    body: &[u8],
    uniform_without_fin: bool,
    max_syms: usize,
) -> (Vec<u8>, u64) {
    let nst = body.len().min(4);
    let mut x = body[..nst].iter().fold(0u64, |x, &b| (x << 8) | b as u64);
    let mut pos = nst;
    let mut out = Vec::new();
    loop {
        while x < L {
            x = (x << 8) | *body.get(pos).unwrap_or(&0) as u64;
            pos += 1;
        }
        if pos >= body.len() && x == L {
            return (out, x);
        }
        let slot = (x & 0xffff) as u32;
        let (v, f, c) = if uniform_without_fin {
            let n = table.symbols() as u32;
            let f = M / n;
            (slot / f, f, (slot / f) * f)
        } else {
            match table.lookup(slot) {
                None => return (out, x),
                Some(v) => (v as u32, table.freq(v), table.cum[v]),
            }
        };
        x = f as u64 * (x >> 16) + (slot - c) as u64;
        out.push(v as u8);
        if out.len() > max_syms {
            return (out, x);
        }
    }
}

#[test]
fn rans_primitive_round_trip_all_models() {
    let mut rng = Rng::new(1);
    for q in 0..=MAX_Q {
        for w in 1..=MAX_WIDTH {
            let t = models().unwrap().get(q, w);
            for len in [0usize, 1, 2, 5, 37, 300] {
                let syms: Vec<u8> = (0..len)
                    .map(|_| {
                        if rng.below(2) == 0 {
                            rng.sample_from(t)
                        } else {
                            rng.below(1 << w) as u8
                        }
                    })
                    .collect();
                let body = encode_body(t, &syms);
                assert!(*body.last().unwrap() != 0 || body.len() == 1);
                let (back, end) = decode_record(t, &body, false, syms.len() + 1);
                assert_eq!(back, syms, "q={q} w={w} len={len}");
                assert_eq!(end, L);
            }
        }
    }
}

#[test]
fn q0_model_needs_the_fin_slot() {
    // q = 0, width 3: symbol 0 owns slots 1..8191, slot 0 is FIN.
    let t = models().unwrap().get(0, 3);
    let syms = vec![4u8, 0, 4, 4];
    let body = encode_body(t, &syms);
    let (back, end) = decode_record(t, &body, false, syms.len() + 1);
    assert_eq!((back, end), (syms.clone(), L));
    // Read as a pure uniform model, the same record does not come back to L
    // with the same symbols.
    let (back_u, end_u) = decode_record(t, &body, true, syms.len() + 1);
    assert!(
        !(back_u == syms && end_u == L),
        "pure uniform model must not fit"
    );

    // Same thing through the scheduler: a q = 0 record holding symbol 0.
    let p = CodecParams::with_widths(vec![3]);
    let recs = vec![vec![Record {
        q: 0,
        reduction: 0,
        syms: syms.clone(),
    }]];
    let stream = interleave(&p, &recs, 4).unwrap();
    assert_eq!(decode_lanes(&stream, &p, 32).unwrap(), vec![syms]);
}

fn random_stream(rng: &mut Rng, p: &CodecParams, nsym: usize) -> (Vec<Vec<u8>>, Vec<u8>) {
    let lanes: Vec<Vec<u8>> = p
        .lane_widths
        .iter()
        .map(|&w| random_lane(rng, w, nsym))
        .collect();
    let recs: Vec<Vec<Record>> = lanes
        .iter()
        .zip(&p.lane_widths)
        .map(|(l, &w)| random_records(rng, w, l))
        .collect();
    let stream = interleave(p, &recs, nsym).unwrap();
    (lanes, stream)
}

#[test]
fn bc1_lanes_round_trip() {
    let p = CodecParams::bc1();
    let mut rng = Rng::new(2);
    for _ in 0..150 {
        let nsym = 1 + rng.below(300) as usize;
        let (lanes, stream) = random_stream(&mut rng, &p, nsym);
        assert_eq!(decode_lanes(&stream, &p, 8 * nsym).unwrap(), lanes);
    }
}

#[test]
fn many_lanes_round_trip() {
    // More lanes than reads per round: exercises the stall rule heavily.
    let p = CodecParams::with_widths(vec![4, 4, 8, 8, 8, 8, 8, 7, 7, 5, 6, 8, 8, 8, 8, 8]);
    let mut rng = Rng::new(3);
    for _ in 0..60 {
        let nsym = 1 + rng.below(200) as usize;
        let (lanes, stream) = random_stream(&mut rng, &p, nsym);
        assert_eq!(decode_lanes(&stream, &p, 8 * nsym).unwrap(), lanes);
    }
}

#[test]
fn extended_length_records() {
    let p = CodecParams::with_widths(vec![8, 8]);
    let mut rng = Rng::new(4);
    let nsym = 1200;
    let lanes: Vec<Vec<u8>> = (0..2)
        .map(|_| (0..nsym).map(|_| rng.below(256) as u8).collect())
        .collect();
    // q = 15 is saturated: random symbols cost about 2 bytes each.
    let recs: Vec<Vec<Record>> = lanes
        .iter()
        .map(|l| {
            vec![Record {
                q: 15,
                reduction: 0,
                syms: l.clone(),
            }]
        })
        .collect();
    let stream = interleave(&p, &recs, nsym).unwrap();
    assert!(stream.len() > 2 * 256);
    assert_eq!(decode_lanes(&stream, &p, 8 * nsym).unwrap(), lanes);
}

#[test]
fn bc1_blocks_round_trip() {
    let mut rng = Rng::new(5);
    let mut tables = Bc1Tables::default();
    for k in (1..256).rev() {
        let j = rng.below(k as u64 + 1) as usize;
        tables.quadrant_codes.swap(k, j);
    }
    for e in tables.history.iter_mut() {
        *e = Some([
            rng.below(32) as u8,
            rng.below(64) as u8,
            rng.below(32) as u8,
            3,
            7,
            1,
        ]);
    }
    for _ in 0..20 {
        let nb = 1 + rng.below(500) as usize;
        let mut blocks = vec![0u8; 8 * nb];
        rng.fill(&mut blocks);
        let distances: Vec<usize> = (0..nb).map(|_| rng.below(256) as usize).collect();
        let lanes = lanes_for_blocks(&blocks, &distances, &tables).unwrap();
        let p = CodecParams::bc1();
        let recs: Vec<Vec<Record>> = lanes
            .iter()
            .zip(&p.lane_widths)
            .map(|(l, &w)| random_records(&mut rng, w, l))
            .collect();
        let stream = interleave(&p, &recs, nb).unwrap();
        assert_eq!(decode_bc1(&stream, 8 * nb, &tables).unwrap(), blocks);
    }
}

#[test]
fn stream_errors() {
    let p = CodecParams::bc1();
    let mut rng = Rng::new(6);
    let nsym = 64;
    let (_, stream) = random_stream(&mut rng, &p, nsym);
    let n = 8 * nsym;
    // Truncated input.
    assert!(decode_lanes(&stream[..stream.len() - 1], &p, n).is_err());
    // Extra trailing byte.
    let mut longer = stream.clone();
    longer.push(0x42);
    assert_eq!(
        decode_lanes(&longer, &p, n),
        Err(Error::TrailingBytes {
            consumed: stream.len(),
            total: stream.len() + 1
        })
    );
    // Wrong output size.
    assert!(decode_lanes(&stream, &p, n + 8).is_err());
    assert!(decode_lanes(&stream, &p, n - 8).is_err());
}

#[test]
fn corrupted_streams_never_panic() {
    let p = CodecParams::bc1();
    let mut rng = Rng::new(8);
    let nsym = 40;
    let (lanes, stream) = random_stream(&mut rng, &p, nsym);
    let mut rejected = 0;
    for i in 0..stream.len() {
        for flip in [0x01u8, 0x10, 0xff] {
            let mut s = stream.clone();
            s[i] ^= flip;
            match decode_lanes(&s, &p, 8 * nsym) {
                Err(_) => rejected += 1,
                // rANS has no checksum: some flips still give a well-formed
                // stream (other symbols, or two model indices with the same
                // table at a small width). They must decode without panicking.
                Ok(l) => assert_eq!(l.len(), lanes.len()),
            }
        }
    }
    // The end-of-record and consumption checks catch most corruptions.
    assert!(
        rejected * 10 >= stream.len() * 3 * 8,
        "{rejected} of {}",
        stream.len() * 3
    );
}

#[test]
fn non_flushed_record_continues_state() {
    // Mode bit 4 clear: the record ends when its bytes are exhausted and the
    // next record (which has no state bytes) keeps feeding the same state.
    let mut rng = Rng::new(21);
    let mut tried = 0;
    for _ in 0..300 {
        let w = 1 + rng.below(8) as u32;
        let q = 1 + rng.below(14) as u32;
        let n = 1 + rng.below(120) as usize;
        let t = models().unwrap().get(q, w);
        let syms: Vec<u8> = (0..n).map(|_| rng.sample_from(t)).collect();
        let len = encode_body(t, &syms).len();
        if len < 2 {
            continue;
        }
        let cut = 1 + rng.below(len as u64 - 1) as usize;
        let stream = split_record(w, q, &syms, cut);
        let p = CodecParams::with_widths(vec![w]);
        assert_eq!(decode_lanes(&stream, &p, 8 * n).unwrap(), vec![syms]);
        tried += 1;
    }
    assert!(tried > 200);
}

#[test]
fn non_flushed_records_in_bc7_layout() {
    // Same split inside a 16-lane stream, so that carried states go through
    // the read cap and the stall rule.
    let p = CodecParams::bc7();
    let mut rng = Rng::new(22);
    for _ in 0..40 {
        let nsym = 2 + rng.below(150) as usize;
        let mut raw = Vec::new();
        let mut want = Vec::new();
        for &w in &p.lane_widths {
            let q = 1 + rng.below(14) as u32;
            let t = models().unwrap().get(q, w);
            let syms: Vec<u8> = (0..nsym).map(|_| rng.sample_from(t)).collect();
            let len = encode_body(t, &syms).len();
            let bytes = if len >= 2 && rng.below(2) == 0 {
                split_record(w, q, &syms, 1 + rng.below(len as u64 - 1) as usize)
            } else {
                encode_record(
                    w,
                    &Record {
                        q,
                        reduction: 0,
                        syms: syms.clone(),
                    },
                )
            };
            raw.push(bytes);
            want.push(syms);
        }
        let stream = interleave_raw(&p, raw, &want, nsym).unwrap();
        assert_eq!(decode_lanes(&stream, &p, 8 * nsym).unwrap(), want);
    }
}
