//! 最小可用 JSON：解析 + 转义。零依赖，只用 std。
//!
//! 只实现桥接口真正用得到的部分：对象、数组、字符串、数字、布尔、null。
//! 解析器带深度上限，坏输入返回 `Err` 而不是 panic。

use std::fmt::Write as _;

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Value>),
    Obj(Vec<(String, Value)>),
}

const MAX_DEPTH: usize = 96;

impl Value {
    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Obj(map) => map.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    /// 按路径取值，例如 `v.at(&["limits", "maxQueue"])`
    pub fn at(&self, path: &[&str]) -> Option<&Value> {
        let mut cur = self;
        for key in path {
            cur = cur.get(key)?;
        }
        Some(cur)
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s.as_str()),
            _ => None,
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Value::Num(n) => Some(*n),
            // 有些网关把数字写成字符串，这里顺手兼容
            Value::Str(s) => s.trim().parse::<f64>().ok(),
            _ => None,
        }
    }

    pub fn as_i64(&self) -> Option<i64> {
        self.as_f64().map(|f| f as i64)
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(*b),
            _ => None,
        }
    }

    pub fn as_arr(&self) -> Option<&[Value]> {
        match self {
            Value::Arr(v) => Some(v.as_slice()),
            _ => None,
        }
    }

    pub fn as_obj(&self) -> Option<&[(String, Value)]> {
        match self {
            Value::Obj(m) => Some(m.as_slice()),
            _ => None,
        }
    }

    pub fn str_at(&self, path: &[&str]) -> Option<&str> {
        self.at(path).and_then(|v| v.as_str())
    }

    pub fn i64_at(&self, path: &[&str]) -> Option<i64> {
        self.at(path).and_then(|v| v.as_i64())
    }

    pub fn bool_at(&self, path: &[&str]) -> Option<bool> {
        self.at(path).and_then(|v| v.as_bool())
    }

    pub fn arr_at(&self, path: &[&str]) -> Option<&[Value]> {
        self.at(path).and_then(|v| v.as_arr())
    }

    /// 取字符串数组（非字符串元素直接跳过，不报错）
    pub fn str_arr_at(&self, path: &[&str]) -> Vec<String> {
        match self.at(path).and_then(|v| v.as_arr()) {
            Some(items) => items
                .iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect(),
            None => Vec::new(),
        }
    }
}

/* ------------------------------- 解析 ------------------------------- */

pub fn parse(text: &str) -> Result<Value, String> {
    let bytes = text.as_bytes();
    let mut p = Parser { b: bytes, i: 0 };
    p.skip_ws();
    let v = p.value(0)?;
    p.skip_ws();
    if p.i < bytes.len() {
        return Err(format!("尾部有多余内容（第 {} 字节）", p.i));
    }
    Ok(v)
}

struct Parser<'a> {
    b: &'a [u8],
    i: usize,
}

impl<'a> Parser<'a> {
    fn peek(&self) -> Option<u8> {
        self.b.get(self.i).copied()
    }

    fn skip_ws(&mut self) {
        while let Some(c) = self.peek() {
            if c == b' ' || c == b'\t' || c == b'\r' || c == b'\n' {
                self.i += 1;
            } else {
                break;
            }
        }
    }

    fn expect_word(&mut self, word: &str) -> Result<(), String> {
        let end = self.i + word.len();
        if end <= self.b.len() && &self.b[self.i..end] == word.as_bytes() {
            self.i = end;
            Ok(())
        } else {
            Err(format!("期望 {}（第 {} 字节）", word, self.i))
        }
    }

    fn value(&mut self, depth: usize) -> Result<Value, String> {
        if depth > MAX_DEPTH {
            return Err("JSON 嵌套过深".to_string());
        }
        self.skip_ws();
        match self.peek() {
            None => Err("JSON 提前结束".to_string()),
            Some(b'{') => self.object(depth),
            Some(b'[') => self.array(depth),
            Some(b'"') => self.string().map(Value::Str),
            Some(b't') => {
                self.expect_word("true")?;
                Ok(Value::Bool(true))
            }
            Some(b'f') => {
                self.expect_word("false")?;
                Ok(Value::Bool(false))
            }
            Some(b'n') => {
                self.expect_word("null")?;
                Ok(Value::Null)
            }
            Some(c) if c == b'-' || c.is_ascii_digit() => self.number(),
            Some(c) => Err(format!("非法字符 {:?}（第 {} 字节）", c as char, self.i)),
        }
    }

    fn object(&mut self, depth: usize) -> Result<Value, String> {
        self.i += 1; // {
        let mut out: Vec<(String, Value)> = Vec::new();
        self.skip_ws();
        if self.peek() == Some(b'}') {
            self.i += 1;
            return Ok(Value::Obj(out));
        }
        loop {
            self.skip_ws();
            if self.peek() != Some(b'"') {
                return Err(format!("对象键必须是字符串（第 {} 字节）", self.i));
            }
            let key = self.string()?;
            self.skip_ws();
            if self.peek() != Some(b':') {
                return Err(format!("对象键后缺少冒号（第 {} 字节）", self.i));
            }
            self.i += 1;
            let val = self.value(depth + 1)?;
            out.push((key, val));
            self.skip_ws();
            match self.peek() {
                Some(b',') => {
                    self.i += 1;
                }
                Some(b'}') => {
                    self.i += 1;
                    return Ok(Value::Obj(out));
                }
                _ => return Err(format!("对象未闭合（第 {} 字节）", self.i)),
            }
        }
    }

    fn array(&mut self, depth: usize) -> Result<Value, String> {
        self.i += 1; // [
        let mut out: Vec<Value> = Vec::new();
        self.skip_ws();
        if self.peek() == Some(b']') {
            self.i += 1;
            return Ok(Value::Arr(out));
        }
        loop {
            let val = self.value(depth + 1)?;
            out.push(val);
            self.skip_ws();
            match self.peek() {
                Some(b',') => {
                    self.i += 1;
                }
                Some(b']') => {
                    self.i += 1;
                    return Ok(Value::Arr(out));
                }
                _ => return Err(format!("数组未闭合（第 {} 字节）", self.i)),
            }
        }
    }

    fn string(&mut self) -> Result<String, String> {
        self.i += 1; // 开引号
        let mut out = String::new();
        loop {
            match self.peek() {
                None => return Err("字符串未闭合".to_string()),
                Some(b'"') => {
                    self.i += 1;
                    return Ok(out);
                }
                Some(b'\\') => {
                    self.i += 1;
                    let esc = match self.peek() {
                        Some(c) => c,
                        None => return Err("转义序列被截断".to_string()),
                    };
                    self.i += 1;
                    match esc {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            let hi = self.hex4()?;
                            let ch = if (0xD800..0xDC00).contains(&hi) {
                                // 代理对：后面必须跟 \uDC00-\uDFFF
                                if self.peek() == Some(b'\\') && self.b.get(self.i + 1) == Some(&b'u')
                                {
                                    self.i += 2;
                                    let lo = self.hex4()?;
                                    if (0xDC00..0xE000).contains(&lo) {
                                        let cp = 0x10000u32
                                            + (((hi as u32) - 0xD800) << 10)
                                            + ((lo as u32) - 0xDC00);
                                        char::from_u32(cp).unwrap_or('\u{fffd}')
                                    } else {
                                        '\u{fffd}'
                                    }
                                } else {
                                    '\u{fffd}'
                                }
                            } else {
                                char::from_u32(hi as u32).unwrap_or('\u{fffd}')
                            };
                            out.push(ch);
                        }
                        other => return Err(format!("未知转义 \\{}", other as char)),
                    }
                }
                Some(c) if c < 0x20 => {
                    return Err(format!("字符串里有裸控制字符 0x{:02x}", c));
                }
                Some(_) => {
                    // 直接复制一段 UTF-8 原始字节（多字节字符整体走这里）
                    let start = self.i;
                    let mut end = start;
                    while end < self.b.len() {
                        let c = self.b[end];
                        if c == b'"' || c == b'\\' || c < 0x20 {
                            break;
                        }
                        end += 1;
                    }
                    if end == start {
                        return Err("字符串解析卡住".to_string());
                    }
                    match std::str::from_utf8(&self.b[start..end]) {
                        Ok(s) => out.push_str(s),
                        Err(_) => {
                            // 非法 UTF-8 按替换字符处理，别让整条消息丢掉
                            out.push_str(&String::from_utf8_lossy(&self.b[start..end]));
                        }
                    }
                    self.i = end;
                }
            }
        }
    }

    fn hex4(&mut self) -> Result<u16, String> {
        if self.i + 4 > self.b.len() {
            return Err("\\u 转义被截断".to_string());
        }
        let mut v: u16 = 0;
        for k in 0..4 {
            let c = self.b[self.i + k];
            let d = match c {
                b'0'..=b'9' => c - b'0',
                b'a'..=b'f' => c - b'a' + 10,
                b'A'..=b'F' => c - b'A' + 10,
                _ => return Err("\\u 后面不是十六进制".to_string()),
            };
            v = (v << 4) | d as u16;
        }
        self.i += 4;
        Ok(v)
    }

    fn number(&mut self) -> Result<Value, String> {
        let start = self.i;
        if self.peek() == Some(b'-') {
            self.i += 1;
        }
        while let Some(c) = self.peek() {
            if c.is_ascii_digit() {
                self.i += 1;
            } else {
                break;
            }
        }
        if self.peek() == Some(b'.') {
            self.i += 1;
            while let Some(c) = self.peek() {
                if c.is_ascii_digit() {
                    self.i += 1;
                } else {
                    break;
                }
            }
        }
        if let Some(c) = self.peek() {
            if c == b'e' || c == b'E' {
                self.i += 1;
                if let Some(s) = self.peek() {
                    if s == b'+' || s == b'-' {
                        self.i += 1;
                    }
                }
                while let Some(c) = self.peek() {
                    if c.is_ascii_digit() {
                        self.i += 1;
                    } else {
                        break;
                    }
                }
            }
        }
        let raw = std::str::from_utf8(&self.b[start..self.i]).unwrap_or("");
        raw.parse::<f64>()
            .map(Value::Num)
            .map_err(|_| format!("非法数字 {:?}", raw))
    }
}

/* ------------------------------- 生成 ------------------------------- */

/// 转义成 JSON 字符串内容（不带外层引号）
pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out
}

/// 带外层引号的 JSON 字符串
pub fn quoted(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    out.push_str(&escape(s));
    out.push('"');
    out
}

/// 数字转 JSON 字面量：整数不带小数点
pub fn num_str(n: f64) -> String {
    if !n.is_finite() {
        return "null".to_string();
    }
    if n.fract() == 0.0 && n.abs() < 9.0e15 {
        format!("{}", n as i64)
    } else {
        format!("{}", n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_flat_object() {
        let v = parse(r#"{"ok":true,"chrome":false,"uptime":12}"#).expect("should parse");
        assert_eq!(v.bool_at(&["ok"]), Some(true));
        assert_eq!(v.bool_at(&["chrome"]), Some(false));
        assert_eq!(v.i64_at(&["uptime"]), Some(12));
    }

    #[test]
    fn parse_nested_and_paths() {
        let raw = r#"{"ok":true,"limits":{"maxQueue":4,"minGapMs":3000},"sessions":["main","设计"]}"#;
        let v = parse(raw).expect("should parse");
        assert_eq!(v.i64_at(&["limits", "maxQueue"]), Some(4));
        assert_eq!(v.i64_at(&["limits", "minGapMs"]), Some(3000));
        assert_eq!(v.str_arr_at(&["sessions"]), vec!["main", "设计"]);
        assert!(v.get("missing").is_none());
    }

    #[test]
    fn parse_unicode_escapes_and_surrogates() {
        let v = parse(r#""a\u4f60b\ud83d\ude00c""#).expect("should parse");
        assert_eq!(v.as_str(), Some("a你b😀c"));
    }

    #[test]
    fn parse_escapes() {
        let v = parse(r#""line1\nline2\t\"q\"\\""#).expect("should parse");
        assert_eq!(v.as_str(), Some("line1\nline2\t\"q\"\\"));
    }

    #[test]
    fn parse_numbers() {
        let v = parse("[-1, 2.5, 1e3, -0.25]").expect("should parse");
        let arr = v.as_arr().expect("array");
        assert_eq!(arr.len(), 4);
        assert_eq!(arr[0].as_f64(), Some(-1.0));
        assert_eq!(arr[1].as_f64(), Some(2.5));
        assert_eq!(arr[2].as_f64(), Some(1000.0));
        assert_eq!(arr[3].as_f64(), Some(-0.25));
    }

    #[test]
    fn parse_empty_containers() {
        assert_eq!(parse("{}").expect("obj"), Value::Obj(vec![]));
        assert_eq!(parse("[]").expect("arr"), Value::Arr(vec![]));
        assert_eq!(parse("null").expect("null"), Value::Null);
    }

    #[test]
    fn parse_rejects_garbage() {
        assert!(parse("{").is_err());
        assert!(parse("{\"a\":}").is_err());
        assert!(parse("[1,2").is_err());
        assert!(parse("nul").is_err());
        assert!(parse("{} trailing").is_err());
        assert!(parse("").is_err());
    }

    #[test]
    fn parse_deep_nesting_does_not_overflow() {
        let mut s = String::new();
        for _ in 0..5000 {
            s.push('[');
        }
        assert!(parse(&s).is_err());
    }

    #[test]
    fn escape_roundtrip() {
        let raw = "换行\n引号\"反斜杠\\制表\t";
        let quoted_s = quoted(raw);
        let back = parse(&quoted_s).expect("roundtrip");
        assert_eq!(back.as_str(), Some(raw));
    }

    #[test]
    fn escape_control_chars() {
        assert_eq!(escape("\u{1}"), "\\u0001");
        assert_eq!(num_str(12.0), "12");
        assert_eq!(num_str(2.5), "2.5");
        assert_eq!(num_str(f64::NAN), "null");
    }

    #[test]
    fn parse_chat_chunk_shape() {
        let raw = r#"{"id":"chatcmpl-1","object":"chat.completion.chunk","choices":[{"index":0,"delta":{"content":"你好"},"finish_reason":null}]}"#;
        let v = parse(raw).expect("chunk");
        let delta = v
            .at(&["choices"])
            .and_then(|c| c.as_arr())
            .and_then(|c| c.first())
            .and_then(|c| c.get("delta"));
        assert_eq!(delta.and_then(|d| d.get("content")).and_then(|c| c.as_str()), Some("你好"));
    }
}
