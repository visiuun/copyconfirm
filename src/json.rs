pub enum J {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<J>),
    Obj(Vec<(String, J)>),
}

impl J {
    pub fn parse(text: &str) -> Option<J> {
        let mut p = Parser { b: text.as_bytes(), i: 0 };
        let value = p.value(0)?;
        p.ws();
        (p.i == p.b.len()).then_some(value)
    }

    pub fn get(&self, key: &str) -> Option<&J> {
        match self {
            J::Obj(fields) => fields.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn num(&self, key: &str) -> Option<f64> {
        match self.get(key)? {
            J::Num(n) => Some(*n),
            _ => None,
        }
    }

    pub fn str(&self, key: &str) -> Option<&str> {
        match self.get(key)? {
            J::Str(s) => Some(s),
            _ => None,
        }
    }

    pub fn bool(&self, key: &str) -> Option<bool> {
        match self.get(key)? {
            J::Bool(b) => Some(*b),
            _ => None,
        }
    }
}

struct Parser<'a> {
    b: &'a [u8],
    i: usize,
}

impl Parser<'_> {
    fn ws(&mut self) {
        while matches!(self.b.get(self.i), Some(b' ' | b'\n' | b'\r' | b'\t')) {
            self.i += 1;
        }
    }

    fn eat(&mut self, lit: &str) -> bool {
        let hit = self.b[self.i..].starts_with(lit.as_bytes());
        if hit {
            self.i += lit.len();
        }
        hit
    }

    fn value(&mut self, depth: u32) -> Option<J> {
        if depth > 32 {
            return None;
        }
        self.ws();
        match *self.b.get(self.i)? {
            b'{' => {
                self.i += 1;
                let mut fields = Vec::new();
                self.ws();
                if self.eat("}") {
                    return Some(J::Obj(fields));
                }
                loop {
                    self.ws();
                    let key = self.string()?;
                    self.ws();
                    if !self.eat(":") {
                        return None;
                    }
                    fields.push((key, self.value(depth + 1)?));
                    self.ws();
                    if self.eat("}") {
                        return Some(J::Obj(fields));
                    }
                    if !self.eat(",") {
                        return None;
                    }
                }
            }
            b'[' => {
                self.i += 1;
                let mut items = Vec::new();
                self.ws();
                if self.eat("]") {
                    return Some(J::Arr(items));
                }
                loop {
                    items.push(self.value(depth + 1)?);
                    self.ws();
                    if self.eat("]") {
                        return Some(J::Arr(items));
                    }
                    if !self.eat(",") {
                        return None;
                    }
                }
            }
            b'"' => self.string().map(J::Str),
            b't' => self.eat("true").then_some(J::Bool(true)),
            b'f' => self.eat("false").then_some(J::Bool(false)),
            b'n' => self.eat("null").then_some(J::Null),
            _ => {
                let start = self.i;
                while matches!(self.b.get(self.i), Some(b'0'..=b'9' | b'-' | b'+' | b'.' | b'e' | b'E')) {
                    self.i += 1;
                }
                std::str::from_utf8(&self.b[start..self.i]).ok()?.parse().ok().map(J::Num)
            }
        }
    }

    fn hex4(&mut self) -> Option<u32> {
        let digits = self.b.get(self.i..self.i + 4)?;
        self.i += 4;
        u32::from_str_radix(std::str::from_utf8(digits).ok()?, 16).ok()
    }

    fn string(&mut self) -> Option<String> {
        if !self.eat("\"") {
            return None;
        }
        let mut out = Vec::new();
        loop {
            let c = *self.b.get(self.i)?;
            self.i += 1;
            match c {
                b'"' => return String::from_utf8(out).ok(),
                b'\\' => {
                    let e = *self.b.get(self.i)?;
                    self.i += 1;
                    let ch = match e {
                        b'"' => '"',
                        b'\\' => '\\',
                        b'/' => '/',
                        b'b' => '\u{8}',
                        b'f' => '\u{c}',
                        b'n' => '\n',
                        b'r' => '\r',
                        b't' => '\t',
                        b'u' => {
                            let hi = self.hex4()?;
                            let code = if (0xD800..0xDC00).contains(&hi) && self.eat("\\u") {
                                0x10000 + ((hi - 0xD800) << 10) + (self.hex4()?.wrapping_sub(0xDC00) & 0x3FF)
                            } else {
                                hi
                            };
                            char::from_u32(code).unwrap_or('\u{FFFD}')
                        }
                        _ => return None,
                    };
                    let mut buf = [0; 4];
                    out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                }
                _ => out.push(c),
            }
        }
    }
}

pub fn quote(out: &mut String, s: &str) {
    use std::fmt::Write;
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_python_output() {
        let j = J::parse(r#"[{"a": "x•😀\u0000", "b": 1794189203.17, "c": true, "d": null}]"#).unwrap();
        let J::Arr(items) = j else { panic!() };
        assert_eq!(items[0].str("a"), Some("x\u{2022}\u{1F600}\0"));
        assert_eq!(items[0].num("b"), Some(1794189203.17));
        assert_eq!(items[0].bool("c"), Some(true));
    }

    #[test]
    fn round_trips_awkward_strings() {
        let original = "line\n\"quoted\" \\ tab\t \u{1} \u{1F600}";
        let mut out = String::new();
        quote(&mut out, original);
        match J::parse(&out) {
            Some(J::Str(s)) => assert_eq!(s, original),
            _ => panic!(),
        }
    }

    #[test]
    fn rejects_garbage_without_panicking() {
        for bad in ["", "{", "[1,", "\"abc", "{\"a\"}", "tru", "\"\\u12", "[[[["] {
            assert!(J::parse(bad).is_none(), "{bad}");
        }
        assert!(J::parse(&"[".repeat(5000)).is_none());
    }
}
