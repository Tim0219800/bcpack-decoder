//! BC1 block assembly from the 12 decoded lanes.
//!
//! For block `i` (stream order = chunk memory order):
//!
//! * lane 0 `v` gives the back-reference distance `D` (folded on one byte):
//!   `v = 0` -> no reference (predictor 0); `v` even -> `D = v / 2` (1..=127);
//!   `v` odd -> `D = 255 - (v >> 1)` (128..=255);
//! * the reference is block `i - D`, or history entry `-(D - i)` when
//!   `D > i`;
//! * lanes 2-7 are zigzag deltas (even = positive) added channel by channel
//!   to the reference endpoints, modulo 32 (R, B) or 64 (G): lanes 2-4 give
//!   c0 = (R, G, B), lanes 5-7 give c1;
//! * lanes 8-11 are the 2 x 2 selector quadrants (top-left, top-right,
//!   bottom-left, bottom-right) through the `quadrant_codes` table.

use crate::error::{Error, Result};
use crate::lanes::{decode_lanes, CodecParams};
use crate::tables::Bc1Tables;

/// Modulus of each endpoint channel `[r0, g0, b0, r1, g1, b1]`.
pub const ENDPOINT_MOD: [i32; 6] = [32, 64, 32, 32, 64, 32];

/// Lane-0 symbol -> back-reference distance (0 = no reference).
pub fn unfold_distance(v: u8) -> usize {
    let v = v as usize;
    if v % 2 == 0 {
        v / 2
    } else {
        255 - (v >> 1)
    }
}

/// Distance (0..=255) -> lane-0 symbol; inverse of [`unfold_distance`].
pub fn fold_distance(d: usize) -> u8 {
    assert!(d <= 255, "distance out of range");
    if d <= 127 {
        (2 * d) as u8
    } else {
        (2 * (255 - d) + 1) as u8
    }
}

/// Zigzag symbol -> signed delta (0, -1, 1, -2, 2, ... for 0, 1, 2, 3, 4).
pub fn zigzag_decode(v: u8) -> i32 {
    let v = v as i32;
    if v % 2 == 0 {
        v / 2
    } else {
        -(v / 2) - 1
    }
}

/// Signed delta -> zigzag symbol; inverse of [`zigzag_decode`].
pub fn zigzag_encode(d: i32) -> u8 {
    if d >= 0 {
        (2 * d) as u8
    } else {
        (-2 * d - 1) as u8
    }
}

/// Endpoints `[r0, g0, b0, r1, g1, b1]` of a BC1 block.
pub fn block_endpoints(b: &[u8]) -> [u8; 6] {
    let c0 = u16::from_le_bytes([b[0], b[1]]);
    let c1 = u16::from_le_bytes([b[2], b[3]]);
    [
        (c0 >> 11) as u8,
        ((c0 >> 5) & 63) as u8,
        (c0 & 31) as u8,
        (c1 >> 11) as u8,
        ((c1 >> 5) & 63) as u8,
        (c1 & 31) as u8,
    ]
}

/// The four quadrant selector bytes (TL, TR, BL, BR) of a BC1 block.
pub fn block_quadrants(b: &[u8]) -> [u8; 4] {
    [
        (b[4] & 15) | ((b[5] & 15) << 4),
        (b[4] >> 4) | ((b[5] >> 4) << 4),
        (b[6] & 15) | ((b[7] & 15) << 4),
        (b[6] >> 4) | ((b[7] >> 4) << 4),
    ]
}

/// Builds one BC1 block from endpoints and quadrant selector bytes.
pub fn make_block(e: &[i32; 6], q: &[u8; 4]) -> [u8; 8] {
    let c0 = (e[0] << 11) | (e[1] << 5) | e[2];
    let c1 = (e[3] << 11) | (e[4] << 5) | e[5];
    [
        (c0 & 255) as u8,
        (c0 >> 8) as u8,
        (c1 & 255) as u8,
        (c1 >> 8) as u8,
        (q[0] & 15) | ((q[1] & 15) << 4),
        (q[0] >> 4) | ((q[1] >> 4) << 4),
        (q[2] & 15) | ((q[3] & 15) << 4),
        (q[2] >> 4) | ((q[3] >> 4) << 4),
    ]
}

/// Assembles BC1 blocks from 12 decoded lanes.
pub fn assemble_bc1(lanes: &[Vec<u8>], tables: &Bc1Tables) -> Result<Vec<u8>> {
    if lanes.len() != 12 {
        return Err(Error::InvalidParams(format!(
            "BC1 needs 12 lanes, got {}",
            lanes.len()
        )));
    }
    let nb = lanes[0].len();
    if lanes.iter().any(|l| l.len() != nb) {
        return Err(Error::InvalidParams("lanes have different lengths".into()));
    }
    let mut ep = vec![[0i32; 6]; nb];
    let mut out = vec![0u8; 8 * nb];
    for i in 0..nb {
        if lanes[1][i] != 0 {
            return Err(Error::UnsupportedLane1 {
                block: i,
                value: lanes[1][i],
            });
        }
        let d = unfold_distance(lanes[0][i]);
        let base: [i32; 6] = if d == 0 {
            [0; 6]
        } else if d > i {
            let slot = d - i;
            let e = tables.history[slot - 1].ok_or(Error::MissingHistorySlot { block: i, slot })?;
            e.map(|c| c as i32)
        } else {
            ep[i - d]
        };
        for c in 0..6 {
            let dl = zigzag_decode(lanes[2 + c][i]);
            ep[i][c] = (base[c] + dl).rem_euclid(ENDPOINT_MOD[c]);
        }
        let q = [
            tables.quadrant_codes[lanes[8][i] as usize],
            tables.quadrant_codes[lanes[9][i] as usize],
            tables.quadrant_codes[lanes[10][i] as usize],
            tables.quadrant_codes[lanes[11][i] as usize],
        ];
        out[8 * i..8 * i + 8].copy_from_slice(&make_block(&ep[i], &q));
    }
    Ok(out)
}

/// Decodes one BC1 BCPack chunk into `out_size` bytes of standard BC1 blocks,
/// in the chunk's memory order (no detiling is applied).
pub fn decode_bc1(raw: &[u8], out_size: usize, tables: &Bc1Tables) -> Result<Vec<u8>> {
    let lanes = decode_lanes(raw, &CodecParams::bc1(), out_size)?;
    assemble_bc1(&lanes, tables)
}

/// Inverse of [`assemble_bc1`] for a chosen distance per block: returns the
/// 12 lanes that rebuild `blocks`. `distances[i]` must be 0 or reference an
/// earlier block or a defined history entry. Used by tests and by the
/// table learner's self-checks.
pub fn lanes_for_blocks(
    blocks: &[u8],
    distances: &[usize],
    tables: &Bc1Tables,
) -> Result<Vec<Vec<u8>>> {
    let nb = blocks.len() / 8;
    if blocks.len() % 8 != 0 || distances.len() != nb {
        return Err(Error::InvalidParams(
            "blocks / distances size mismatch".into(),
        ));
    }
    let mut inverse = [0u8; 256];
    for (s, &c) in tables.quadrant_codes.iter().enumerate() {
        inverse[c as usize] = s as u8;
    }
    let mut lanes = vec![vec![0u8; nb]; 12];
    for i in 0..nb {
        let b = &blocks[8 * i..8 * i + 8];
        let e = block_endpoints(b);
        let d = distances[i];
        if d > 255 {
            return Err(Error::InvalidParams(format!("distance {d} > 255")));
        }
        let base: [u8; 6] = if d == 0 {
            [0; 6]
        } else if d > i {
            tables.history[d - i - 1].ok_or(Error::MissingHistorySlot {
                block: i,
                slot: d - i,
            })?
        } else {
            block_endpoints(&blocks[8 * (i - d)..8 * (i - d) + 8])
        };
        lanes[0][i] = fold_distance(d);
        for c in 0..6 {
            let m = ENDPOINT_MOD[c];
            let mut delta = (e[c] as i32 - base[c] as i32).rem_euclid(m);
            if delta >= m / 2 {
                delta -= m;
            }
            lanes[2 + c][i] = zigzag_encode(delta);
        }
        for (k, q) in block_quadrants(b).iter().enumerate() {
            lanes[8 + k][i] = inverse[*q as usize];
        }
    }
    Ok(lanes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testenc::Rng;

    fn synthetic_tables(rng: &mut Rng) -> Bc1Tables {
        let mut t = Bc1Tables::default();
        for k in (1..256).rev() {
            let j = rng.below(k as u64 + 1) as usize;
            t.quadrant_codes.swap(k, j);
        }
        for e in t.history.iter_mut() {
            *e = Some([
                rng.below(32) as u8,
                rng.below(64) as u8,
                rng.below(32) as u8,
                rng.below(32) as u8,
                rng.below(64) as u8,
                rng.below(32) as u8,
            ]);
        }
        t
    }

    #[test]
    fn distance_folding() {
        assert_eq!(unfold_distance(0), 0);
        assert_eq!(unfold_distance(2), 1);
        assert_eq!(unfold_distance(254), 127);
        assert_eq!(unfold_distance(255), 128);
        assert_eq!(unfold_distance(1), 255);
        for d in 0..=255 {
            assert_eq!(unfold_distance(fold_distance(d)), d);
        }
    }

    #[test]
    fn zigzag() {
        assert_eq!(
            (0..5).map(zigzag_decode).collect::<Vec<_>>(),
            vec![0, -1, 1, -2, 2]
        );
        for d in -32..32 {
            assert_eq!(zigzag_decode(zigzag_encode(d)), d);
        }
    }

    #[test]
    fn quadrant_layout() {
        // Selector of pixel (row r, column c) is bits 2c..2c+1 of byte 4 + r.
        let mut b = [0u8; 8];
        b[4] = 0b11_10_01_00; // row 0: 0,1,2,3
        b[5] = 0b00_01_10_11; // row 1: 3,2,1,0
        let q = block_quadrants(&b);
        // TL = (0,0)=0 (0,1)=1 (1,0)=3 (1,1)=2
        assert_eq!(q[0], (1 << 2) | (3 << 4) | (2 << 6));
        assert_eq!(make_block(&[0; 6], &q)[4..6], b[4..6]);
    }

    #[test]
    fn rebuilds_blocks_from_synthetic_lanes() {
        let mut rng = Rng::new(7);
        let tables = synthetic_tables(&mut rng);
        let nb = 600;
        let mut blocks = vec![0u8; 8 * nb];
        rng.fill(&mut blocks);
        // Mix of no reference, near references, far references, and history.
        let distances: Vec<usize> = (0..nb)
            .map(|i| match rng.below(4) {
                0 => 0,
                1 if i > 0 => 1 + rng.below(i.min(255) as u64) as usize,
                _ => 1 + rng.below(255) as usize,
            })
            .collect();
        let lanes = lanes_for_blocks(&blocks, &distances, &tables).unwrap();
        assert_eq!(assemble_bc1(&lanes, &tables).unwrap(), blocks);
    }

    #[test]
    fn hand_built_chain() {
        let mut t = Bc1Tables::default();
        t.history[2] = Some([10, 20, 30, 1, 2, 3]); // virtual block -3
        let mut lanes = vec![vec![0u8; 2]; 12];
        // block 0: D = 3 -> history slot -3, deltas +1 / -1
        lanes[0][0] = fold_distance(3);
        for c in 0..3 {
            lanes[2 + c][0] = zigzag_encode(1);
            lanes[5 + c][0] = zigzag_encode(-2);
        }
        // block 1: D = 1 -> block 0, delta 0 except R0 wraps: 11 + 21 = 32 -> 0
        lanes[0][1] = fold_distance(1);
        lanes[2][1] = zigzag_encode(21);
        let out = assemble_bc1(&lanes, &t).unwrap();
        assert_eq!(block_endpoints(&out[0..8]), [11, 21, 31, 31, 0, 1]);
        assert_eq!(block_endpoints(&out[8..16]), [0, 21, 31, 31, 0, 1]);
    }

    #[test]
    fn assembly_errors() {
        let t = Bc1Tables::default();
        let mut lanes = vec![vec![0u8; 1]; 12];
        lanes[0][0] = fold_distance(5);
        assert_eq!(
            assemble_bc1(&lanes, &t),
            Err(Error::MissingHistorySlot { block: 0, slot: 5 })
        );
        let mut lanes = vec![vec![0u8; 1]; 12];
        lanes[1][0] = 3;
        assert_eq!(
            assemble_bc1(&lanes, &t),
            Err(Error::UnsupportedLane1 { block: 0, value: 3 })
        );
        assert!(assemble_bc1(&lanes[..11], &t).is_err());
    }
}
