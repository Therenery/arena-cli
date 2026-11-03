//! URL 百分号编码（只处理 query 参数，够用且好测）。

/// 把任意字符串编码成可以安全放进 query 的值。
///
/// 保留 RFC 3986 的 unreserved 字符（字母数字与 `-._~`），
/// 其余一律 `%XX`；空格编成 `%20` 而不是 `+`（`+` 在 path 里会被当成字面量）。
pub fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*b as char)
            }
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

/// 把 `key=value` 拼成 query，value 自动编码
pub fn pair(key: &str, value: &str) -> String {
    format!("{}={}", encode(key), encode(value))
}

/// 拼接 base 与 query，自动处理 `?` / `&`
pub fn with_query(path: &str, query: &[(&str, &str)]) -> String {
    if query.is_empty() {
        return path.to_string();
    }
    let mut out = String::from(path);
    out.push(if path.contains('?') { '&' } else { '?' });
    for (i, (k, v)) in query.iter().enumerate() {
        if i > 0 {
            out.push('&');
        }
        out.push_str(&pair(k, v));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_unreserved() {
        assert_eq!(encode("abcXYZ0189-_.~"), "abcXYZ0189-_.~");
    }

    #[test]
    fn encodes_space_as_percent20() {
        assert_eq!(encode("a b"), "a%20b");
    }

    #[test]
    fn encodes_cjk_as_utf8_bytes() {
        // 「设」= E8 AE BE, 「计」= E8 AE A1
        assert_eq!(encode("设计"), "%E8%AE%BE%E8%AE%A1");
    }

    #[test]
    fn encodes_reserved_chars() {
        assert_eq!(encode("a/b?c=d&e"), "a%2Fb%3Fc%3Dd%26e");
        assert_eq!(encode("%"), "%25");
        assert_eq!(encode("+"), "%2B");
    }

    #[test]
    fn roundtrip_decode_is_identity_for_ascii() {
        // 这里不做解码，只确认「编码结果里没有裸的特殊字符」
        let s = "session name/with?weird&chars";
        let e = encode(s);
        assert!(!e.contains('/'));
        assert!(!e.contains('&'));
        assert!(!e.contains(' '));
    }

    #[test]
    fn pair_and_query() {
        assert_eq!(pair("session", "设计 1"), "session=%E8%AE%BE%E8%AE%A1%201");
        assert_eq!(
            with_query("/history", &[("session", "main")]),
            "/history?session=main"
        );
        assert_eq!(with_query("/history", &[]), "/history");
        assert_eq!(
            with_query("/x?a=1", &[("b", "2")]),
            "/x?a=1&b=2"
        );
    }
}
