//! Command-line front end.

use bcpack_decoder::lanes::{decode_lanes, CodecParams};
use bcpack_decoder::learn::{self, LearnOptions, RefKind};
use bcpack_decoder::model::{models, MAX_Q, MAX_WIDTH};
use bcpack_decoder::{decode_bc1, Bc1Tables, Error};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

const USAGE: &str = "\
bcpack-decoder: clean-room decoder for Xbox Series DirectStorage BCPack streams

USAGE:
  bcpack-decoder decode --codec bc1 --tables TABLES.json --size N IN.bin OUT.bin
      Decode one BC1 chunk into N bytes of standard BC1 blocks (chunk memory order).

  bcpack-decoder lanes (--codec C | --widths W0,W1,...) --size N IN.bin OUT.bin
      Decode the entropy layer only and write the lane symbols, lane after lane
      (N / 8 bytes per lane). C is bc1, bc3, bc4, bc5, bc6h or bc7. Needs no
      table file; --widths allows experiments with other lane layouts.

  bcpack-decoder learn-tables --list LIST.txt --ref blocks|quads [--tables IN.json]
                              [--corpus CORPUS.txt] --out OUT.json
      Derive the BC1 table file from your own data. LIST.txt has one sample per
      line: `CHUNK.bin SIZE REFERENCE`. See README.md, section Tables.

  bcpack-decoder model --q Q --width W
      Print the cumulative frequency table of model (Q, W).

OPTIONS:
  -h, --help     Show this help
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

struct Args {
    flags: Vec<(String, String)>,
    positional: Vec<String>,
}

impl Args {
    fn parse(args: &[String]) -> Result<Self, Error> {
        let mut flags = Vec::new();
        let mut positional = Vec::new();
        let mut it = args.iter();
        while let Some(a) = it.next() {
            if let Some(name) = a.strip_prefix("--") {
                let v = it
                    .next()
                    .ok_or_else(|| Error::InvalidParams(format!("--{name} needs a value")))?;
                flags.push((name.to_string(), v.clone()));
            } else {
                positional.push(a.clone());
            }
        }
        Ok(Args { flags, positional })
    }
    fn get(&self, name: &str) -> Option<&str> {
        self.flags
            .iter()
            .rev()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }
    fn need(&self, name: &str) -> Result<&str, Error> {
        self.get(name)
            .ok_or_else(|| Error::InvalidParams(format!("missing --{name}")))
    }
    fn number(&self, name: &str) -> Result<usize, Error> {
        self.need(name)?
            .parse()
            .map_err(|_| Error::InvalidParams(format!("--{name} must be a non-negative integer")))
    }
    fn io_pair(&self) -> Result<(&str, &str), Error> {
        match self.positional.as_slice() {
            [a, b] => Ok((a, b)),
            _ => Err(Error::InvalidParams(
                "expected IN and OUT file arguments".into(),
            )),
        }
    }
}

fn read(path: &str) -> Result<Vec<u8>, Error> {
    std::fs::read(path).map_err(|e| Error::Io(format!("{path}: {e}")))
}

fn write(path: &str, data: &[u8]) -> Result<(), Error> {
    std::fs::write(path, data).map_err(|e| Error::Io(format!("{path}: {e}")))
}

fn run(argv: &[String]) -> Result<(), Error> {
    let Some(cmd) = argv.first() else {
        print!("{USAGE}");
        return Ok(());
    };
    if cmd == "-h" || cmd == "--help" || cmd == "help" {
        print!("{USAGE}");
        return Ok(());
    }
    let a = Args::parse(&argv[1..])?;
    match cmd.as_str() {
        "decode" => {
            let codec = a.need("codec")?;
            if codec != "bc1" {
                return Err(Error::InvalidParams(format!(
                    "codec {codec:?} is not supported (only bc1 is solved)"
                )));
            }
            let tables = Bc1Tables::load(Path::new(a.need("tables")?))?;
            let size = a.number("size")?;
            let (input, output) = a.io_pair()?;
            let blocks = decode_bc1(&read(input)?, size, &tables)?;
            write(output, &blocks)?;
            eprintln!("decoded {} blocks", blocks.len() / 8);
            Ok(())
        }
        "lanes" => {
            let params = match (a.get("codec"), a.get("widths")) {
                (Some(c), None) => CodecParams::for_codec(c).ok_or_else(|| {
                    Error::InvalidParams(format!(
                        "unknown codec {c:?} (bc1, bc3, bc4, bc5, bc6h or bc7)"
                    ))
                })?,
                (None, Some(w)) => {
                    let widths = w
                        .split(',')
                        .map(|x| x.trim().parse::<u32>())
                        .collect::<Result<Vec<_>, _>>()
                        .map_err(|_| Error::InvalidParams("bad --widths list".into()))?;
                    CodecParams::with_widths(widths)
                }
                _ => {
                    return Err(Error::InvalidParams(
                        "give either --codec or --widths".into(),
                    ))
                }
            };
            let size = a.number("size")?;
            let (input, output) = a.io_pair()?;
            let lanes = decode_lanes(&read(input)?, &params, size)?;
            write(output, &lanes.concat())?;
            eprintln!(
                "decoded {} lanes x {} symbols",
                lanes.len(),
                size / params.unit_bytes
            );
            Ok(())
        }
        "learn-tables" => {
            let kind = match a.need("ref")? {
                "blocks" => RefKind::Blocks,
                "quads" => RefKind::Quads,
                other => {
                    return Err(Error::InvalidParams(format!(
                        "--ref must be blocks or quads, not {other}"
                    )))
                }
            };
            let opts = LearnOptions {
                list: PathBuf::from(a.need("list")?),
                kind,
                initial: a.get("tables").map(PathBuf::from),
                corpus: a.get("corpus").map(PathBuf::from),
            };
            let out = a.need("out")?;
            let (tables, report) = learn::learn_from_files(&opts)?;
            write(out, tables.to_json().as_bytes())?;
            eprint!("{report}");
            Ok(())
        }
        "model" => {
            let q = a.number("q")? as u32;
            let w = a.number("width")? as u32;
            if q > MAX_Q || w == 0 || w > MAX_WIDTH {
                return Err(Error::InvalidParams(
                    "need 0 <= q <= 15 and 1 <= width <= 8".into(),
                ));
            }
            let t = models()?.get(q, w);
            println!(
                "# q = {q}, width = {w}: slot 0 = FIN; symbol v owns slots [cum[v], cum[v+1])"
            );
            for v in 0..t.symbols() {
                println!("{v}\t{}\t{}", t.cum[v], t.freq(v));
            }
            Ok(())
        }
        other => Err(Error::InvalidParams(format!(
            "unknown command {other:?}; try --help"
        ))),
    }
}
