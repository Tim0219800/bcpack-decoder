//! Error type shared by every layer of the decoder.

use std::fmt;

/// Everything that can go wrong while decoding or loading tables.
///
/// The decoder never panics on malformed input: every inconsistency in the
/// stream is reported as one of these variants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// The codec parameters are unusable (no lanes, width out of range, ...).
    InvalidParams(String),
    /// The requested output size is not a whole number of coding units.
    OutSizeNotMultiple { out_size: usize, unit: usize },
    /// A lane asked for a byte after the end of the input.
    UnexpectedEof { lane: usize, offset: usize },
    /// A record selects a symbol width outside 1..=8.
    WidthOutOfRange { lane: usize, width: i32 },
    /// A lane state fell to zero, which a valid encoder cannot produce.
    ZeroState { lane: usize },
    /// The FIN slot (slot 0) was hit while the record still had bytes left.
    FinBeforeEnd { lane: usize, symbol: usize },
    /// A record ran out of bytes but its state is not the initial encoder state.
    BadFragmentEnd { lane: usize, state: u64 },
    /// A lane already has all its symbols but its current record is not finished.
    ExtraSymbols { lane: usize },
    /// The scheduler did not converge within the round limit.
    RoundLimit { rounds: usize },
    /// The stream ended with bytes that no lane consumed.
    TrailingBytes { consumed: usize, total: usize },
    /// A lane ended with the wrong number of symbols or with unread bytes.
    LaneIncomplete {
        lane: usize,
        symbols: usize,
        expected: usize,
    },
    /// The built-in probability model failed its own consistency checks.
    Model(String),
    /// A table file is malformed or inconsistent.
    Tables(String),
    /// A block references a history slot that the loaded table file leaves undefined.
    MissingHistorySlot { block: usize, slot: usize },
    /// Lane 1 of a BC1 stream carried a non-zero symbol (semantics unknown).
    UnsupportedLane1 { block: usize, value: u8 },
    /// JSON syntax error.
    Json(String),
    /// I/O error (message only, so that the type stays `Clone`).
    Io(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use Error::*;
        match self {
            InvalidParams(m) => write!(f, "invalid codec parameters: {m}"),
            OutSizeNotMultiple { out_size, unit } => {
                write!(f, "output size {out_size} is not a multiple of the {unit}-byte unit")
            }
            UnexpectedEof { lane, offset } => {
                write!(f, "lane {lane} needs a byte past the end of the input (offset {offset})")
            }
            WidthOutOfRange { lane, width } => {
                write!(f, "lane {lane}: symbol width {width} outside 1..=8")
            }
            ZeroState { lane } => write!(f, "lane {lane}: rANS state reached zero"),
            FinBeforeEnd { lane, symbol } => {
                write!(f, "lane {lane}: FIN slot hit at symbol {symbol} before the record end")
            }
            BadFragmentEnd { lane, state } => write!(
                f,
                "lane {lane}: record ended with state 0x{state:x} instead of 0x1000000"
            ),
            ExtraSymbols { lane } => {
                write!(f, "lane {lane}: record holds more symbols than the output needs")
            }
            RoundLimit { rounds } => write!(f, "scheduler did not finish within {rounds} rounds"),
            TrailingBytes { consumed, total } => {
                write!(f, "only {consumed} of {total} input bytes were consumed")
            }
            LaneIncomplete { lane, symbols, expected } => write!(
                f,
                "lane {lane} ended with {symbols} symbols (expected {expected}) or unread bytes"
            ),
            Model(m) => write!(f, "probability model error: {m}"),
            Tables(m) => write!(f, "table file error: {m}"),
            MissingHistorySlot { block, slot } => write!(
                f,
                "block {block} references history slot -{slot}, which the table file leaves undefined"
            ),
            UnsupportedLane1 { block, value } => write!(
                f,
                "block {block}: lane 1 symbol {value} is non-zero (semantics unknown)"
            ),
            Json(m) => write!(f, "JSON error: {m}"),
            Io(m) => write!(f, "I/O error: {m}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e.to_string())
    }
}

/// Crate-wide result alias.
pub type Result<T> = std::result::Result<T, Error>;
