//! 极简 JSON 解析 / 序列化（零依赖，只为对接桥接的 HTTP API 够用）。

#[derive(Debug, Clone, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

impl Json {
    pub fn get(&self, key: &str) -> Option<&Json> {
        if let Json::Obj(m) = self {
            m.iter().find(|(k, _)| k == key).map(|(_, v)| v)
        } else {
            None
        }
    }
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Json::Str(s) => Some(s),
            _ => None,
        }
    }
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Json::Num(n) => Some(*n),
            _ => None,
        }
    }
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Json::Bool(b) => Some(*b),
            _ => None,
        }
    }
    pub fn as_arr(&self) -> Option<&[Json]> {
        match self {
            Json::Arr(a) => Some(a),
            _ => None,
        }
    }
    pub fn str(s: impl Into<String>) -> Json {
        Json::Str(s.into())
    }
    pub fn obj(pairs: Vec<(&str, Json)>) -> Json {
        Json::Obj(pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
    }

    pub fn to_string(&self) -> String {
        let mut out = String::new();
        self.write(&mut out);
        out
    }

    pub fn pretty(&self) -> String {
        let mut out = String::new();
        self.write_pretty(&mut out, 0);
        out
    }

    fn write(&self, out: &mut String) {
        match self {
            Json::Null => out.push_str("null"),
            Json::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            Json::Num(n) => {
                if n.is_finite() && n.fract() == 0.0 && n.abs() < 1e15 {
                    out.push_str(&format!("{}", *n as i64));
                } else {
                    out.push_str(&format!("{}", n));
                }
            }
            Json::Str(s) => write_str(s, out),
            Json::Arr(a) => {
                out.push('[');
                for (i, v) in a.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    v.write(out);
                }
                out.push(']');
            }
            Json::Obj(m) => {
                out.push('{');
                for (i, (k, v)) in m.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    write_str(k, out);
                    out.push(':');
                    v.write(out);
                }
                out.push('}');
            }
        }
    }

    fn write_pretty(&self, out: &mut String, ind: usize) {
        match self {
            Json::Arr(a) if !a.is_empty() => {
                out.push_str("[\n");
                for (i, v) in a.iter().enumerate() {
                    out.push_str(&"  ".repeat(ind + 1));
                    v.write_pretty(out, ind + 1);
                    if i + 1 < a.len() {
                        out.push(',');
                    }
                    out.push('\n');
                }
                out.push_str(&"  ".repeat(ind));
                out.push(']');
            }
            Json::Obj(m) if !m.is_empty() => {
                out.push_str("{\n");
                for (i, (k, v)) in m.iter().enumerate() {
                    out.push_str(&"  ".repeat(ind + 1));
                    write_str(k, out);
                    out.push_str(": ");
                    v.write_pretty(out, ind + 1);
                    if i + 1 < m.len() {
                        out.push(',');
                    }
                    out.push('\n');
                }
                out.push_str(&"  ".repeat(ind));
                out.push('}');
            }
            _ => self.write(out),
        }
    }

    pub fn parse(input: &str) -> Result<Json, String> {
        let mut p = Parser {
            b: input.as_bytes(),
            i: 0,
        };
        p.skip_ws();
        let v = p.value()?;
        p.skip_ws();
        if p.i != p.b.len() {
            return Err(format!("trailing data at byte {}", p.i));
        }
        Ok(v)
    }
}

fn write_str(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

struct Parser<'a> {
    b: &'a [u8],
    i: usize,
}

impl<'a> Parser<'a> {
    fn skip_ws(&mut self) {
        while self.i < self.b.len() && matches!(self.b[self.i], b' ' | b'\t' | b'\n' | b'\r') {
            self.i += 1;
        }
    }
    fn peek(&self) -> Option<u8> {
        self.b.get(self.i).copied()
    }
    fn value(&mut self) -> Result<Json, String> {
        self.skip_ws();
        match self.peek() {
            None => Err("unexpected end of input".into()),
            Some(b'{') => self.object(),
            Some(b'[') => self.array(),
            Some(b'"') => Ok(Json::Str(self.string()?)),
            Some(b't') => self.lit("true", Json::Bool(true)),
            Some(b'f') => self.lit("false", Json::Bool(false)),
            Some(b'n') => self.lit("null", Json::Null),
            Some(_) => self.number(),
        }
    }
    fn lit(&mut self, s: &str, v: Json) -> Result<Json, String> {
        if self.b[self.i..].starts_with(s.as_bytes()) {
            self.i += s.len();
            Ok(v)
        } else {
            Err(format!("bad literal at byte {}", self.i))
        }
    }
    fn number(&mut self) -> Result<Json, String> {
        let start = self.i;
        while let Some(c) = self.peek() {
            if matches!(c, b'0'..=b'9' | b'-' | b'+' | b'.' | b'e' | b'E') {
                self.i += 1;
            } else {
                break;
            }
        }
        if start == self.i {
            return Err(format!("expected value at byte {}", self.i));
        }
        let s = std::str::from_utf8(&self.b[start..self.i]).map_err(|e| e.to_string())?;
        s.parse::<f64>()
            .map(Json::Num)
            .map_err(|e| format!("bad number '{}': {}", s, e))
    }
    fn string(&mut self) -> Result<String, String> {
        self.i += 1; // opening quote
        let mut out = String::new();
        loop {
            let c = self.peek().ok_or("unterminated string")?;
            self.i += 1;
            match c {
                b'"' => return Ok(out),
                b'\\' => {
                    let e = self.peek().ok_or("unterminated escape")?;
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
                            let h1 = self.hex4()?;
                            let cp = if (0xD800..0xDC00).contains(&h1) {
                                if self.peek() == Some(b'\\')
                                    && self.b.get(self.i + 1) == Some(&b'u')
                                {
                                    self.i += 2;
                                    let h2 = self.hex4()?;
                                    if (0xDC00..0xE000).contains(&h2) {
                                        0x10000 + ((h1 - 0xD800) << 10) + (h2 - 0xDC00)
                                    } else {
                                        0xFFFD
                                    }
                                } else {
                                    0xFFFD
                                }
                            } else {
                                h1
                            };
                            out.push(char::from_u32(cp).unwrap_or('\u{FFFD}'));
                        }
                        _ => return Err(format!("bad escape at byte {}", self.i)),
                    }
                }
                _ => {
                    let mut buf = vec![c];
                    let extra = if c >= 0xF0 {
                        3
                    } else if c >= 0xE0 {
                        2
                    } else if c >= 0xC0 {
                        1
                    } else {
                        0
                    };
                    for _ in 0..extra {
                        if let Some(nb) = self.peek() {
                            self.i += 1;
                            buf.push(nb);
                        }
                    }
                    match std::str::from_utf8(&buf) {
                        Ok(s) => out.push_str(s),
                        Err(_) => out.push('\u{FFFD}'),
                    }
                }
            }
        }
    }
    fn hex4(&mut self) -> Result<u32, String> {
        if self.i + 4 > self.b.len() {
            return Err("bad \\u escape".into());
        }
        let s = std::str::from_utf8(&self.b[self.i..self.i + 4]).map_err(|e| e.to_string())?;
        self.i += 4;
        u32::from_str_radix(s, 16).map_err(|e| format!("bad \\u escape '{}': {}", s, e))
    }
    fn array(&mut self) -> Result<Json, String> {
        self.i += 1; // '['
        let mut out = Vec::new();
        self.skip_ws();
        if self.peek() == Some(b']') {
            self.i += 1;
            return Ok(Json::Arr(out));
        }
        loop {
            out.push(self.value()?);
            self.skip_ws();
            match self.peek() {
                Some(b',') => {
                    self.i += 1;
                }
                Some(b']') => {
                    self.i += 1;
                    return Ok(Json::Arr(out));
                }
                _ => return Err(format!("expected ',' or ']' at byte {}", self.i)),
            }
        }
    }
    fn object(&mut self) -> Result<Json, String> {
        self.i += 1; // '{'
        let mut out = Vec::new();
        self.skip_ws();
        if self.peek() == Some(b'}') {
            self.i += 1;
            return Ok(Json::Obj(out));
        }
        loop {
            self.skip_ws();
            if self.peek() != Some(b'"') {
                return Err(format!("expected object key at byte {}", self.i));
            }
            let k = self.string()?;
            self.skip_ws();
            if self.peek() != Some(b':') {
                return Err(format!("expected ':' at byte {}", self.i));
            }
            self.i += 1;
            let v = self.value()?;
            out.push((k, v));
            self.skip_ws();
            match self.peek() {
                Some(b',') => {
                    self.i += 1;
                }
                Some(b'}') => {
                    self.i += 1;
                    return Ok(Json::Obj(out));
                }
                _ => return Err(format!("expected ',' or '}}' at byte {}", self.i)),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let s = r#"{"a":1,"b":[true,null,"x\ny"],"c":{"d":"中文🎉"},"e":-2.5}"#;
        let j = Json::parse(s).unwrap();
        assert_eq!(j.get("a").unwrap().as_f64(), Some(1.0));
        assert_eq!(j.get("c").unwrap().get("d").unwrap().as_str(), Some("中文🎉"));
        let out = j.to_string();
        let j2 = Json::parse(&out).unwrap();
        assert_eq!(j, j2);
    }

    #[test]
    fn surrogate_pair() {
        let j = Json::parse(r#""\ud83c\udf89""#).unwrap();
        assert_eq!(j.as_str(), Some("🎉"));
    }

    #[test]
    fn escapes() {
        let j = Json::str("a\"b\\c\nd").to_string();
        assert_eq!(j, r#""a\"b\\c\nd""#);
    }
}
