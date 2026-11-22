//! Clean-room decoder for Xbox Series DirectStorage **BCPack** texture streams.
//!
//! BCPack is a lossless entropy coder for BCn texture blocks: the block
//! fields are split into *lanes*, each lane is coded with byte-wise rANS
//! under a static parametric probability model, and the lanes share one byte
//! stream through a deterministic read scheduler.
//!
//! Status: **BC1 is fully supported**. The entropy layer
//! ([`decode_lanes`] with [`CodecParams::for_codec`]) also decodes BC3, BC4,
//! BC5, BC6H and BC7 chunks into lane symbols; their block reconstruction is
//! not implemented.
//!
//! BC1 needs two learned tables that are not shipped with the crate; they are
//! loaded at runtime from a JSON file ([`Bc1Tables`]) that you can derive from
//! your own data with the `learn-tables` command ([`learn`]).
//!
//! ```no_run
//! use bcpack_decoder::{decode_bc1, Bc1Tables};
//! # fn main() -> Result<(), bcpack_decoder::Error> {
//! let tables = Bc1Tables::load("tables.json".as_ref())?;
//! let raw = std::fs::read("chunk.bin")?;
//! let blocks = decode_bc1(&raw, 32768, &tables)?;
//! assert_eq!(blocks.len(), 32768);
//! # Ok(()) }
//! ```
//!
//! See `docs/SPEC.md` for the bitstream specification.

pub mod bc1;
pub mod error;
pub mod json;
pub mod lanes;
pub mod learn;
pub mod model;
pub mod tables;

#[cfg(test)]
mod roundtrip;
#[cfg(test)]
pub(crate) mod testenc;
#[cfg(test)]
mod verilog_ref;

pub use bc1::{assemble_bc1, decode_bc1};
pub use error::{Error, Result};
pub use lanes::{decode_lanes, CodecParams};
pub use tables::Bc1Tables;
