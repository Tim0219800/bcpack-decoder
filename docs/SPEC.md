# BCPack bitstream specification (as reconstructed)

*French version: [SPEC.fr.md](SPEC.fr.md).*

This document describes the Xbox Series DirectStorage BCPack format as far as
it was reconstructed from data (see [Method](#9-method-clean-room-data-only)).
Everything marked **established** reproduces real streams byte for byte;
everything else is labelled as a hypothesis.

Notation: `x >> n`, `x << n`, `&`, `|` as in C. "Byte" = 8 bits. Integers in
record headers are little-endian; rANS states are big-endian.

## 1. Overview

BCPack is a lossless entropy coder for BCn texture blocks. A **chunk** is a
self-contained byte string that decodes to a known number of output bytes
(`out_size`, given by the container). Inside a chunk:

* the block fields are split into `K` **lanes** (12 for BC1); each lane
  carries exactly one symbol per **coding unit** (8 output bytes, i.e. one
  BC1 block);
* each lane is a chain of **records**; each record is coded with byte-wise
  **rANS** under a static model chosen by its header;
* the lanes share a single byte stream; bytes are handed to lanes by a
  deterministic **scheduler** (at most 8 bytes per round, by lane number).

There is no chunk header, no lane directory and no checksum: the first byte
of the chunk is the first header byte of lane 0.

## 2. Record layout (established)

```
mode  len_lo  [len_hi]  state[0 .. min(4, total)]  payload ...
```

`mode` byte:

| bits | meaning |
|------|---------|
| 0-3  | `q`, model index (0..15) |
| 4    | state present; always 1 in real data (a record with bit 4 clear is rejected) |
| 5    | extended length: `len_hi` follows |
| 6-7  | `r`, width reduction: the record's symbol width is `w' = w - r`, where `w` is the lane's base width |

Record size `total` (bytes after the length field, state included):

* short form (bit 5 = 0): `total = len_lo + 1` (1..256);
* extended form (bit 5 = 1): `total = (len_lo | len_hi << 8) + 257`
  (257..65792).

The first `nst = min(4, total)` record bytes are the initial rANS state,
**big-endian**: `x` is the integer formed by these `nst` bytes, most significant first. The remaining
`total - nst` bytes are the payload (the lane's **budget**).

**Trailing zero bytes are not stored.** An encoder drops the zero bytes at the
end of a record (state included, so a state may be 1 to 3 bytes long). The
decoder restores them implicitly: whenever a record has no bytes left and
`x < L`, it shifts in zero bytes (`x <<= 8`) until `x >= L`, at no cost in
rounds.

## 3. rANS coder (established)

* Probability scale `M = 2^16` (16-bit frequencies, tables sum to 65536).
* Normalised state interval `[L, 256 L)` with `L = 2^24`; states fit in 32
  bits.
* Byte-wise renormalisation: while `x < L`, `x = (x << 8) | next_byte`.
* Decoding one symbol from table `cum` (cumulative frequencies):

  ```
  slot = x & 0xFFFF
  find s with cum[s] <= slot < cum[s+1]
  x    = (cum[s+1] - cum[s]) * (x >> 16) + slot - cum[s]
  ```

* The encoder (informative) is the standard byte-wise rANS encoder: it starts
  from `x = L`, encodes the symbols in reverse order, emits the low byte of the
  state while `x >= 2^16 * freq`, and stores the final state big-endian
  followed by the emitted bytes in reverse emission order.

### FIN slot and end of record

Slot 0 of every table has frequency 1 and belongs to no real symbol: it is
the **FIN** slot. Real symbols `v = 0..n-1` occupy slots `cum[0] = 1` to
`cum[n] = 65536`. Because no real symbol owns slot 0, an encoded state never
has `x & 0xFFFF == 0` except at the very end, where `x` is back at the
encoder's initial state `L`.

A record ends when its budget is 0, no fetched byte is waiting, and
`x >= L` with either `x == L` or `slot == 0`. The end is valid only if
`x == L` exactly; anything else is an error. Hitting slot 0 while the record
still has bytes is an error too.

## 4. Probability model (established)

The table for model `q` and width `w'` (`n = 2^w'` symbols) is generated, not
transmitted.

### q = 0: uniform with FIN

`cum[0] = 1`, `cum[v] = v * 2^(16-w')` for `v = 1..n`. Symbol 0 therefore has
`2^(16-w') - 1` slots and every other symbol `2^(16-w')`. This detail
matters: a pure uniform model (symbol 0 owning slot 0) does not decode real
streams.

### q = 1..15: truncated geometric law

With `rho = exp(-lambda_q / n)` and `F(v) = (1 - rho^v) / (1 - rho^n)`:

1. Buckets: `{0}, {1}, {2,3}, {4..7}, ..., {n/2 .. n-1}` (`w' + 1` buckets).
   All symbols of a bucket share one frequency.
2. Boundary at each bucket start: `B_k = round_half_even(65536 * F(start_k))`;
   then force `B_0 = 1` (the FIN slot) and `B_last = 65536`.
3. For buckets `k = last .. 1` (top-down), with `size_k` symbols:
   `B_k = min(B_k, B_{k+1} - size_k)`;
   `f_k = max(1, round_half_even((B_{k+1} - B_k) / size_k))`;
   `B_k = B_{k+1} - f_k * size_k`.
4. Bucket 0 (symbol 0) gets `f_0 = B_1 - B_0`.

Rounding is half-to-even (IEEE `roundTiesToEven`), computed in double
precision.

| q | lambda_q | q | lambda_q | q | lambda_q |
|---|----------|---|----------|---|----------|
| 1 | 0.7399434 | 6 | 5.6759508 | 11 | 31.4462 |
| 2 | 1.4962494 | 7 | 7.50811 | 12 | 53.67977 |
| 3 | 2.30715745 | 8 | 10.1124375 | 13 | 109.2579 |
| 4 | 3.22101565 | 9 | 14.03737 | 14 | 328.076 |
| 5 | 4.3073742 | 10 | 20.35237 | 15 | 3000 (saturated) |

The lambdas are fitted values: each lies inside an interval of admissible
values (all observed records decode), and the integer tables, not the
lambdas, are what matters. Example, `(q, w') = (12, 5)`: per-symbol
frequencies by bucket `53290, 9957, 1104, 14, 1, 1` (FIN = 1; total 65536).
`q = 15` is saturated: every symbol but 0 has frequency 1.

The crate's test `model::tests::table_fingerprint` pins a hash of all
16 x 8 tables, so that a platform with a different `exp`/`pow` would be
detected.

## 5. Scheduler (established)

All lanes start by reading a record header. Each lane keeps:

* `phase`: Header, Run or Done;
* `buf`: at most one fetched byte not yet merged into the state;
* `budget`: payload bytes of the current record not yet fetched;
* `x`: rANS state; `pending`: symbols decoded ahead of the unit counter (0 or 1);
* `fresh`: header completed in this round.

Decoding proceeds in **rounds**. Each round has four steps.

1. **Requests.** A lane *wants* a byte if it is in Header phase, or in Run
   phase with `buf` empty, `budget > 0`, and (`x < L` or `pending == 0`).
   Scanning lanes by increasing number, the first **8** lanes that want a byte
   are served. A Run lane that wanted a byte and was not served is **stalled**
   for this round.
2. **Reads.** Served lanes, in increasing lane number, each take the next
   byte of the stream. A Header lane appends it to its header; when the header
   is complete (section 2) the lane enters Run with `fresh` set. A Run lane
   puts it into `buf` and decrements `budget`.
3. **Decoding.** For each lane in Run phase, in lane order:
   1. if `fresh`: clear it and do nothing else this round;
   2. if `x < L`: if `buf` holds a byte, merge it (`x = x << 8 | buf`) and do
      nothing else this round; else if `budget == 0`, shift in zero bytes
      until `x >= L`; else (waiting for a byte) do nothing else;
   3. if the record has ended (section 3): check `x == L`; the lane becomes
      Done if it has all its symbols, otherwise it starts a new header;
      nothing else this round;
   4. if `pending == 1`: do nothing else;
   5. if the lane already has all its symbols: error (the record is longer
      than the output);
   6. if the lane is stalled: do nothing else (**non-served-lane rule**: a lane
      that asked for a byte and did not get it does not decode in this round,
      even if its state would allow it; served lanes decode normally);
   7. decode one symbol, set `pending = 1`; then, if `x < L` and `buf` holds a
      byte, merge it immediately; if `budget == 0`, `buf` is empty and
      `x < L`, shift in zero bytes; finally apply the end test of step 3.
4. **Unit synchronisation.** If every lane either has `pending == 1` or is
   Done with more symbols than the number of completed units, clear `pending`
   in every lane and count one more completed unit.

Decoding stops when all lanes are Done. A valid chunk then satisfies: every
lane has exactly `out_size / 8` symbols, no lane has unread record bytes, and
every byte of the chunk has been consumed. (The implementation also gives up
after `20 * units + 200` rounds.)

Notes:

* A record header is read one byte per round (mode, length byte(s), state
  bytes), so lanes 8 and up start reading only when fewer than 8 lower lanes
  want a byte.
* Merging a fetched byte costs a round; restoring trimmed zero bytes does not.
* A lane runs at most one symbol ahead of the slowest lane.

## 6. BC1 (established)

Lanes (base widths in parentheses):

| lane | width | content |
|------|-------|---------|
| 0 | 8 | folded back-reference distance |
| 1 | 8 | always 0 in observed data (meaning unknown; the decoder rejects other values) |
| 2, 3, 4 | 5, 6, 5 | zigzag deltas of c0 = (R, G, B) |
| 5, 6, 7 | 5, 6, 5 | zigzag deltas of c1 = (R, G, B) |
| 8, 9, 10, 11 | 8 | selector quadrants: top-left, top-right, bottom-left, bottom-right |

For block `i` (in stream order):

* **Folded distance.** Lane 0 symbol `v`: `v = 0` means no reference (the
  predictor is 0 for every channel); `v` even gives `D = v / 2` (1..127);
  `v` odd gives `D = 255 - (v >> 1)` (128..255). The window is 255 blocks.
* **Reference.** Block `i - D` if `D <= i`; otherwise history entry
  `-(D - i)`.
* **History.** Before block 0 the window holds a static table of 255 endpoint
  pairs (virtual blocks -1 .. -255), identical for every chunk. It does
  **not** contain blocks of the previous chunk. The values are learned
  constants and are not part of this specification (see section 8).
* **Endpoints.** For each channel `c` of `(R0, G0, B0, R1, G1, B1)`:
  `e[i][c] = (e[ref][c] + zigzag(lane[2 + c][i])) mod m_c`, with
  `m = 32, 64, 32, 32, 64, 32` and `zigzag(v) = v / 2` if `v` is even,
  `-(v / 2) - 1` if odd (0, -1, 1, -2, 2, ...). c0 and c1 are predicted
  independently, channel by channel.
* **Colour words.** `c0 = R0 << 11 | G0 << 5 | B0`, `c1` likewise; written
  little-endian in block bytes 0-1 and 2-3.
* **Selectors.** Each index lane symbol maps through a static 256-entry table
  (a permutation, learned, see section 8) to a **quadrant code**: four 2-bit
  selectors of a 2 x 2 pixel quadrant, bits `2p .. 2p+1` for pixel `p` in the
  order (0,0), (0,1), (1,0), (1,1) (row, column) of the quadrant. With
  quadrant codes `TL, TR, BL, BR`:

  ```
  byte 4 = (TL & 15) | (TR & 15) << 4     row 0
  byte 5 = (TL >> 4) | (TR >> 4) << 4     row 1
  byte 6 = (BL & 15) | (BR & 15) << 4     row 2
  byte 7 = (BL >> 4) | (BR >> 4) << 4     row 3
  ```

  Symbols are numbered by decreasing frequency; the most frequent ones are
  the constant quadrants.

**Memory order and tiling.** Block `i` of the stream is output bytes
`8i .. 8i+8` of the chunk. A chunk covers a byte range of the texture as it
lies in GPU memory, i.e. in the tiled (AMD Gfx10 swizzled) layout; BCPack
does not reorder blocks. The back-reference window and the history therefore
work in memory order, not in raster order, and detiling is a separate step
after decoding. A chunk may hold a whole mip level, part of one, or several
small levels.

## 7. Status per format

| format | status |
|--------|--------|
| BC1 | **solved**: entropy layer and block semantics reproduce real chunks exactly |
| BC7 | not solved. Same record format, tables and 8-byte read cap; 16 lanes, base widths `4,4,8,8,8,8,8,7,7,5,6,8,8,8,8,8` (last one assumed), one symbol per lane per 8 bytes (two per BC7 block). With the BC1 scheduling rules decoding desynchronises after a few hundred rounds, typically when a lane restarts after a short `q = 0` record. Semantics not studied. |
| BC5 | not solved. Tentatively 12 lanes (`3,4,8,8` then eight lanes of width 6). |
| BC4, BC3, BC6H | not studied. |

The entropy layer of this crate (`decode_lanes`, CLI `lanes --widths`) is
generic over the lane layout to help further work.

## 8. Table file

BC1 needs two learned tables. They are loaded at runtime from JSON:

```json
{
  "format": "bcpack-tables",
  "version": 1,
  "bc1": {
    "history": [[r0, g0, b0, r1, g1, b1], null, ...],
    "quadrant_codes": [c0, c1, ..., c255]
  }
}
```

* `history`: exactly 255 items; item `k` is the entry of virtual block
  `-(k+1)`, or `null` if unknown (decoding fails only when a block actually
  references an unknown entry). R and B are 0..31, G is 0..63.
* `quadrant_codes`: exactly 256 integers, a permutation of 0..255; item `s`
  is the quadrant code of index symbol `s` (layout in section 6).

The `learn-tables` command derives both tables from your own data; see the
README.

## 9. Method (clean room, data only)

The format was reconstructed only by observing data: statistics on many
thousands of BCPack chunks and on the uncompressed mip levels stored next to
them. No Microsoft binary, game executable, SDK, or console software was
disassembled, decompiled or traced, and no confidential documentation was
used. Public information used: Microsoft's public DirectStorage
documentation (BCPack is an rANS entropy coder for BCn data that separates
endpoints from indices), public discussions by compression developers
(lossless coding on top of BCn blocks), and public notes from independent
researchers (byte-wise rANS with 16-bit probabilities and `L = 2^24`).

Main steps:

1. **Coder.** Exhaustive search over rANS variants (state size, precision,
   `L`, byte order, interleaving). The acceptance criterion is strict: a
   correct configuration decodes a record with the state returning exactly to
   `L` and every byte consumed, which random data satisfies with probability
   about `2^-32`.
2. **Records.** Column statistics on many chunks gave the header layout
   (mode, length, state), confirmed by the lengths summing to chunk sizes.
3. **Probability law.** Small complete records (decodable without knowing the
   schedule) fixed the integer tables bucket by bucket; one parameter per
   `q` explained them all. The lambdas were then tightened until every record
   of every chunk decoded. The `q = 0` FIN rule came from the last failing
   chunks.
4. **Scheduler.** A round-by-round simulator was compared with complete
   chunks; each rule (8 reads per round, lane-number priority, merge costs a
   round, free zero restoration, non-served-lane rule) was kept only if it
   increased the number of exactly decoded chunks without losing any.
5. **Semantics.** Decoded lanes were compared with the next mip level stored
   uncompressed (downsampled correlation), with exact local constraints
   (`c0 > c1` in opaque four-colour blocks; selector 3 unused in three-colour
   blocks), and with neighbour statistics of selector quadrants.
6. **Learned tables.** History entries by least squares against the next mip
   level, then corrected with the exact constraints; the quadrant mapping by
   frequency, mean weight and bigram likelihood against a corpus of plain BC1
   blocks.

Result for BC1: every BC1 chunk of the corpus studied (about 100 000 chunks)
decodes exactly, and the decoded textures agree with the uncompressed lower mip levels stored alongside (median correlation 0.998 after downsampling).
