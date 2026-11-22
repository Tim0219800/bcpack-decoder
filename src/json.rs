//! Minimal JSON reader and writer (std only), enough for the table files.

use crate::error::{Error, Result};

/// A parsed JSON value. Objects keep their key order.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    Array(Vec<Value>),
    Object(Vec<(String, Value)>),
}

impl Value {
    /// Member `key` of an object.
    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Object(m) => m.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }
    pub fn as_array(&self) -> Option<&[Value]> {
        match self {
            Value::Array(a) => Some(a),
            _ => None,
        }
    }
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::String(s) => Some(s),
            _ => None,
        }
    }
    /// The value as a non-negative integer, if it is one.
    pub fn as_u64(&self) -> Option<u64> {
        match *self {
            Value::Number(n) if n >= 0.0 && n.fract() == 0.0 && n <= 9.007_199_254_740_992e15 => {
                Some(n as u64)
            }
            _ => None,
        }
    }
}

/// Parses a complete JSON document.
pub fn parse(text: &str) -> Result<Value> {
    let mut p = Parser {
        s: text.as_bytes(),
        i: 0,
        depth: 0,
    };
    let v = p.value()?;
    p.ws();
    if p.i != p.s.len() {
        return Err(p.err("trailing characters"));
    }
    Ok(v)
}

struct Parser<'a> {
    s: &'a [u8],
    i: usize,
    depth: usize,
}

impl Parser<'_> {
    fn err(&self, m: &str) -> Error {
        Error::Json(format!("{m} at byte {}", self.i))
    }
    fn ws(&mut self) {
        while self.i < self.s.len() && matches!(self.s[self.i], b' ' | b'\t' | b'\n' | b'\r') {
            self.i += 1;
        }
    }
    fn eat(&mut self, c: u8) -> Result<()> {
        self.ws();
        if self.s.get(self.i) == Some(&c) {
            self.i += 1;
            Ok(())
        } else {
            Err(self.err(&format!("expected '{}'", c as char)))
        }
    }
    fn lit(&mut self, word: &str, v: Value) -> Result<Value> {
        if self.s[self.i..].starts_with(word.as_bytes()) {
            self.i += word.len();
            Ok(v)
        } else {
            Err(self.err("invalid literal"))
        }
    }
    fn value(&mut self) -> Result<Value> {
        self.ws();
        self.depth += 1;
        if self.depth > 64 {
            return Err(self.err("nesting too deep"));
        }
        let v = match self.s.get(self.i) {
            None => Err(self.err("unexpected end")),
            Some(b'n') => self.lit("null", Value::Null),
            Some(b't') => self.lit("true", Value::Bool(true)),
            Some(b'f') => self.lit("false", Value::Bool(false)),
            Some(b'"') => self.string().map(Value::String),
            Some(b'[') => {
                self.i += 1;
                let mut a = Vec::new();
                self.ws();
                if self.s.get(self.i) == Some(&b']') {
                    self.i += 1;
                } else {
                    loop {
                        a.push(self.value()?);
                        self.ws();
                        match self.s.get(self.i) {
                            Some(b',') => self.i += 1,
                            Some(b']') => {
                                self.i += 1;
                                break;
                            }
                            _ => return Err(self.err("expected ',' or ']'")),
                        }
                    }
                }
                Ok(Value::Array(a))
            }
            Some(b'{') => {
                self.i += 1;
                let mut m = Vec::new();
                self.ws();
                if self.s.get(self.i) == Some(&b'}') {
                    self.i += 1;
                } else {
                    loop {
                        self.ws();
                        if self.s.get(self.i) != Some(&b'"') {
                            return Err(self.err("expected a key"));
                        }
                        let k = self.string()?;
                        self.eat(b':')?;
                        let v = self.value()?;
                        m.push((k, v));
                        self.ws();
                        match self.s.get(self.i) {
                            Some(b',') => self.i += 1,
                            Some(b'}') => {
                                self.i += 1;
                                break;
                            }
                            _ => return Err(self.err("expected ',' or '}'")),
                        }
                    }
                }
                Ok(Value::Object(m))
            }
            Some(_) => self.number(),
        };
        self.depth -= 1;
        v
    }
    fn string(&mut self) -> Result<String> {
        self.i += 1; // opening quote
        let mut out = String::new();
        loop {
            let c = *self
                .s
                .get(self.i)
                .ok_or_else(|| self.err("unterminated string"))?;
            self.i += 1;
            match c {
                b'"' => return Ok(out),
                b'\\' => {
                    let e = *self.s.get(self.i).ok_or_else(|| self.err("bad escape"))?;
                    self.i += 1;
                    match e {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            let h = self
                                .s
                                .get(self.i..self.i + 4)
                                .ok_or_else(|| self.err("bad \\u"))?;
                            let h = std::str::from_utf8(h).map_err(|_| self.err("bad \\u"))?;
                            let cp = u32::from_str_radix(h, 16).map_err(|_| self.err("bad \\u"))?;
                            self.i += 4;
                            out.push(char::from_u32(cp).unwrap_or('\u{fffd}'));
                        }
                        _ => return Err(self.err("bad escape")),
                    }
                }
                _ => {
                    // Copy one UTF-8 sequence.
                    let start = self.i - 1;
                    let len = match c {
                        0x00..=0x7f => 1,
                        0xc0..=0xdf => 2,
                        0xe0..=0xef => 3,
                        _ => 4,
                    };
                    let end = (start + len).min(self.s.len());
                    let chunk = std::str::from_utf8(&self.s[start..end])
                        .map_err(|_| self.err("invalid UTF-8"))?;
                    out.push_str(chunk);
                    self.i = end;
                }
            }
        }
    }
    fn number(&mut self) -> Result<Value> {
        let start = self.i;
        while self.i < self.s.len()
            && matches!(
                self.s[self.i],
                b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9'
            )
        {
            self.i += 1;
        }
        let t = std::str::from_utf8(&self.s[start..self.i]).map_err(|_| self.err("bad number"))?;
        t.parse::<f64>()
            .map(Value::Number)
            .map_err(|_| self.err("bad number"))
    }
}

/// Escapes a string for JSON output.
pub fn quote(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    o.push('"');
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
            c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_nested_documents() {
        let v = parse(r#" { "a": [1, 2.5, -3e2, null, true], "b": {"c": "x\"A"} } "#).unwrap();
        let a = v.get("a").unwrap().as_array().unwrap();
        assert_eq!(a[0].as_u64(), Some(1));
        assert_eq!(a[1], Value::Number(2.5));
        assert_eq!(a[2], Value::Number(-300.0));
        assert_eq!(a[3], Value::Null);
        assert_eq!(v.get("b").unwrap().get("c").unwrap().as_str(), Some("x\"A"));
        assert_eq!(
            parse(&quote("é\"\n")).unwrap(),
            Value::String("é\"\n".into())
        );
    }

    #[test]
    fn rejects_malformed_documents() {
        for bad in [
            "",
            "[1,",
            "{\"a\" 1}",
            "[1] x",
            "tru",
            "\"abc",
            "{1:2}",
            "[-]",
        ] {
            assert!(parse(bad).is_err(), "{bad:?}");
        }
    }
}
