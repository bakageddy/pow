use serde_json::{json, Map, Value};

pub struct Cursor<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Cursor<'a> {
    pub const fn new(text: &'a str) -> Self {
        Self { bytes: text.as_bytes(), at: 0 }
    }

    pub fn skip_space(&mut self) {
        while self.at < self.bytes.len() && self.bytes[self.at].is_ascii_whitespace() {
            self.at += 1;
        }
    }

    pub fn eat(&mut self, token: &[u8]) -> bool {
        if self.bytes.get(self.at..).is_some_and(|rest| rest.starts_with(token)) {
            self.at += token.len();
            true
        } else {
            false
        }
    }

    pub fn number(&mut self) -> Option<f64> {
        let start = self.at;
        if self.bytes.get(self.at).is_some_and(|b| *b == b'-' || *b == b'+') {
            self.at += 1;
        }
        let mut digits = 0;
        while self.bytes.get(self.at).is_some_and(u8::is_ascii_digit) {
            self.at += 1;
            digits += 1;
        }
        if self.bytes.get(self.at) == Some(&b'.') && self.bytes.get(self.at + 1) != Some(&b'.') {
            self.at += 1;
            while self.bytes.get(self.at).is_some_and(u8::is_ascii_digit) {
                self.at += 1;
                digits += 1;
            }
        }
        if digits == 0 {
            self.at = start;
            return None;
        }
        std::str::from_utf8(&self.bytes[start..self.at]).ok()?.parse().ok()
    }

    pub fn field(&mut self, name: &[u8]) -> Option<f64> {
        self.skip_space();
        if !self.eat(name) {
            return None;
        }
        self.number()
    }
}

/// Converts a finite f64 to a compact JSON value: integer when it has no fractional part.
pub fn f64_to_json(n: f64) -> Value {
    // The guards (>= 0.0, fract() == 0.0, <= u64::MAX as f64) make both casts safe.
    #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    if n.is_finite() && n >= 0.0 && n.fract() == 0.0 && n <= u64::MAX as f64 {
        Value::from(n as u64)
    } else {
        json!(n)
    }
}

/// Inserts `n` into `map` only when it is finite, so NaN/Inf never reach the output.
pub fn insert_finite(map: &mut Map<String, Value>, key: &str, n: f64) {
    if n.is_finite() {
        map.insert(key.to_owned(), f64_to_json(n));
    }
}

/// Retrieves a finite f64 from a JSON object by key, returning None for NaN/Inf.
pub fn get_f64(v: &Value, key: &str) -> Option<f64> {
    v.get(key).and_then(Value::as_f64).filter(|n| n.is_finite())
}

/// Parses the first number from a string, ignoring leading whitespace.
pub fn parse_leading_number(v: &str) -> Option<f64> {
    let mut cursor = Cursor::new(v.trim_start());
    cursor.number()
}
