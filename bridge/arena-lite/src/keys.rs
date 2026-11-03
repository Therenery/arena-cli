//! 按键模型 + VT 序列解析（unix 与 Windows 共用一套 Key 语义）。

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Key {
    Char(char),
    Enter,
    /// 换行但不发送（Shift+Enter / Alt+Enter / Ctrl+J）
    Newline,
    Backspace,
    Delete,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    PageUp,
    PageDown,
    Tab,
    BackTab,
    Esc,
    /// Ctrl + 字母（小写）
    Ctrl(char),
    /// 能识别是按键，但不需要处理（F 键、鼠标、Insert 等）
    Ignored,
}

/// 增量解析字节流里的按键（VT/xterm 序列）。
///
/// Unix 靠它把 `\x1b[A` 这类序列解成方向键；Windows 用 ReadConsoleInputW
/// 直接拿键记录，用不上，所以这里按平台裁剪，避免 Windows 上有死代码警告。
#[cfg(unix)]
#[derive(Default)]
pub struct KeyParser {
    buf: Vec<u8>,
}

#[cfg(unix)]
impl KeyParser {
    pub fn new() -> Self {
        KeyParser { buf: Vec::new() }
    }

    pub fn push(&mut self, bytes: &[u8], out: &mut Vec<Key>) {
        self.buf.extend_from_slice(bytes);
        self.drain(out);
    }

    /// 一段时间没有新字节：把孤立的 ESC 判定成 Esc 键
    pub fn timeout(&mut self, out: &mut Vec<Key>) {
        if self.buf.len() == 1 && self.buf[0] == 0x1b {
            self.buf.clear();
            out.push(Key::Esc);
        }
    }

    fn drain(&mut self, out: &mut Vec<Key>) {
        let mut i = 0usize;
        while i < self.buf.len() {
            match parse_one(&self.buf[i..]) {
                Step::Key(k, used) => {
                    out.push(k);
                    i += used;
                }
                Step::NeedMore => break,
                Step::Skip(used) => {
                    i += used;
                }
            }
        }
        if i > 0 {
            self.buf.drain(..i);
        }
    }
}

#[cfg(unix)]
enum Step {
    Key(Key, usize),
    NeedMore,
    Skip(usize),
}

#[cfg(unix)]
fn parse_one(b: &[u8]) -> Step {
    let first = match b.first() {
        Some(x) => *x,
        None => return Step::NeedMore,
    };
    match first {
        0x1b => parse_escape(b),
        0x0d => Step::Key(Key::Enter, 1),
        0x0a => Step::Key(Key::Newline, 1),
        0x09 => Step::Key(Key::Tab, 1),
        0x7f => Step::Key(Key::Backspace, 1),
        0x08 => Step::Key(Key::Backspace, 1),
        0x03 => Step::Key(Key::Ctrl('c'), 1),
        0x01..=0x1a => {
            let ch = (b'a' + (first - 1)) as char;
            Step::Key(Key::Ctrl(ch), 1)
        }
        0x00 => Step::Skip(1),
        0x20..=0x7e => Step::Key(Key::Char(first as char), 1),
        0x80..=0xff => match std::str::from_utf8(&b[..b.len().min(4)]) {
            Ok(s) => match s.chars().next() {
                Some(ch) => Step::Key(Key::Char(ch), ch.len_utf8()),
                None => Step::Skip(1),
            },
            Err(e) => {
                if e.valid_up_to() == 0 && e.error_len().is_none() {
                    Step::NeedMore
                } else if e.valid_up_to() > 0 {
                    match std::str::from_utf8(&b[..e.valid_up_to()]) {
                        Ok(s) => match s.chars().next() {
                            Some(ch) => Step::Key(Key::Char(ch), ch.len_utf8()),
                            None => Step::Skip(1),
                        },
                        Err(_) => Step::Skip(1),
                    }
                } else {
                    Step::Skip(1)
                }
            }
        },
        _ => Step::Skip(1),
    }
}

#[cfg(unix)]
fn parse_escape(b: &[u8]) -> Step {
    if b.len() == 1 {
        return Step::NeedMore;
    }
    match b[1] {
        b'[' => {
            // CSI：参数 + 终止字节（0x40..=0x7e）
            let mut end = 2usize;
            while end < b.len() {
                let c = b[end];
                if (0x40..=0x7e).contains(&c) {
                    break;
                }
                end += 1;
            }
            if end >= b.len() {
                return Step::NeedMore;
            }
            let params = &b[2..end];
            let fin = b[end];
            let key = csi_key(params, fin);
            Step::Key(key, end + 1)
        }
        b'O' => {
            if b.len() < 3 {
                return Step::NeedMore;
            }
            let key = match b[2] {
                b'A' => Key::Up,
                b'B' => Key::Down,
                b'C' => Key::Right,
                b'D' => Key::Left,
                b'H' => Key::Home,
                b'F' => Key::End,
                b'P' => Key::Ignored,
                b'Q' => Key::Ignored,
                b'R' => Key::Ignored,
                b'S' => Key::Ignored,
                _ => Key::Ignored,
            };
            Step::Key(key, 3)
        }
        // Alt+Enter / Alt+换行
        b'\r' | b'\n' => Step::Key(Key::Newline, 2),
        0x1b => Step::Key(Key::Esc, 1),
        _ => {
            // Alt+字母：当普通字符处理
            match b[1] {
                c if c.is_ascii_graphic() => Step::Key(Key::Char(c as char), 2),
                _ => Step::Key(Key::Esc, 1),
            }
        }
    }
}

#[cfg(unix)]
fn csi_key(params: &[u8], fin: u8) -> Key {
    let text = String::from_utf8_lossy(params).to_string();
    // CSI-u（Windows Terminal / kitty 的扩展）：13;2u 是 Shift+Enter，13;3u 是 Alt+Enter
    if fin == b'u' {
        let mut it = text.split(';');
        let code = it.next().unwrap_or("").parse::<u32>().unwrap_or(0);
        let mods = it.next().and_then(|s| s.parse::<u32>().ok()).unwrap_or(1);
        if code == 13 {
            if mods >= 2 {
                return Key::Newline;
            }
            return Key::Enter;
        }
        if let Some(c) = char::from_u32(code) {
            if mods == 5 || mods == 6 {
                // Ctrl+字母
                let low = c.to_ascii_lowercase();
                if low.is_ascii_alphabetic() {
                    return Key::Ctrl(low);
                }
            }
            return Key::Char(c);
        }
        return Key::Ignored;
    }
    match fin {
        b'A' => Key::Up,
        b'B' => Key::Down,
        b'C' => Key::Right,
        b'D' => Key::Left,
        b'H' => Key::Home,
        b'F' => Key::End,
        b'Z' => Key::BackTab,
        b'~' => {
            let code = text
                .split(';')
                .next()
                .unwrap_or("")
                .parse::<u32>()
                .unwrap_or(0);
            match code {
                1 | 7 => Key::Home,
                2 => Key::Ignored, // Insert
                3 => Key::Delete,
                4 | 8 => Key::End,
                5 => Key::PageUp,
                6 => Key::PageDown,
                11..=14 => Key::Ignored, // F1-F4
                15 => Key::Ignored,
                17..=21 => Key::Ignored, // F6-F10
                23 | 24 => Key::Ignored, // F11/F12
                _ => Key::Ignored,
            }
        }
        b'u' => Key::Ignored,
        _ => Key::Ignored,
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn parse_all(bytes: &[u8]) -> Vec<Key> {
        let mut p = KeyParser::new();
        let mut out = Vec::new();
        p.push(bytes, &mut out);
        out
    }

    #[test]
    fn plain_chars() {
        assert_eq!(parse_all(b"abc"), vec![
            Key::Char('a'),
            Key::Char('b'),
            Key::Char('c')
        ]);
    }

    #[test]
    fn enter_vs_newline() {
        assert_eq!(parse_all(b"\r"), vec![Key::Enter]);
        assert_eq!(parse_all(b"\n"), vec![Key::Newline]);
        assert_eq!(parse_all(b"\x1b\r"), vec![Key::Newline]); // Alt+Enter
    }

    #[test]
    fn backspace_both_codes() {
        assert_eq!(parse_all(b"\x7f"), vec![Key::Backspace]);
        assert_eq!(parse_all(b"\x08"), vec![Key::Backspace]);
    }

    #[test]
    fn arrows() {
        assert_eq!(
            parse_all(b"\x1b[A\x1b[B\x1b[C\x1b[D"),
            vec![Key::Up, Key::Down, Key::Right, Key::Left]
        );
        // SS3 变体
        assert_eq!(parse_all(b"\x1bOA"), vec![Key::Up]);
    }

    #[test]
    fn paging_and_delete() {
        assert_eq!(
            parse_all(b"\x1b[5~\x1b[6~\x1b[3~\x1b[H\x1b[F"),
            vec![Key::PageUp, Key::PageDown, Key::Delete, Key::Home, Key::End]
        );
    }

    #[test]
    fn ctrl_keys() {
        assert_eq!(parse_all(b"\x03"), vec![Key::Ctrl('c')]);
        assert_eq!(parse_all(b"\x12"), vec![Key::Ctrl('r')]);
        assert_eq!(parse_all(b"\x14"), vec![Key::Ctrl('t')]);
        assert_eq!(parse_all(b"\x0c"), vec![Key::Ctrl('l')]);
        assert_eq!(parse_all(b"\x0a"), vec![Key::Newline]); // Ctrl+J 换行
    }

    #[test]
    fn utf8_chars() {
        assert_eq!(parse_all("你好".as_bytes()), vec![
            Key::Char('你'),
            Key::Char('好')
        ]);
    }

    #[test]
    fn utf8_split_across_reads() {
        let bytes = "好".as_bytes(); // 3 字节
        let mut p = KeyParser::new();
        let mut out = Vec::new();
        p.push(&bytes[..1], &mut out);
        assert!(out.is_empty());
        p.push(&bytes[1..], &mut out);
        assert_eq!(out, vec![Key::Char('好')]);
    }

    #[test]
    fn escape_sequence_split_across_reads() {
        let mut p = KeyParser::new();
        let mut out = Vec::new();
        p.push(b"\x1b", &mut out);
        assert!(out.is_empty());
        p.push(b"[", &mut out);
        assert!(out.is_empty());
        p.push(b"5~", &mut out);
        assert_eq!(out, vec![Key::PageUp]);
    }

    #[test]
    fn lone_esc_becomes_escape_on_timeout() {
        let mut p = KeyParser::new();
        let mut out = Vec::new();
        p.push(b"\x1b", &mut out);
        assert!(out.is_empty());
        p.timeout(&mut out);
        assert_eq!(out, vec![Key::Esc]);
    }

    #[test]
    fn shift_enter_csi_u() {
        assert_eq!(parse_all(b"\x1b[13;2u"), vec![Key::Newline]);
        assert_eq!(parse_all(b"\x1b[13;3u"), vec![Key::Newline]);
        assert_eq!(parse_all(b"\x1b[13u"), vec![Key::Enter]);
    }

    #[test]
    fn tab_and_backtab() {
        assert_eq!(parse_all(b"\t"), vec![Key::Tab]);
        assert_eq!(parse_all(b"\x1b[Z"), vec![Key::BackTab]);
    }

    #[test]
    fn mixed_stream() {
        let mut p = KeyParser::new();
        let mut out = Vec::new();
        p.push("a你\x1b[A\r".as_bytes(), &mut out);
        assert_eq!(out, vec![
            Key::Char('a'),
            Key::Char('你'),
            Key::Up,
            Key::Enter
        ]);
    }

    #[test]
    fn unknown_sequences_are_ignored_not_panicking() {
        assert_eq!(parse_all(b"\x1b[?1;2c"), vec![Key::Ignored]);
        assert_eq!(parse_all(b"\x1b[200~hi\x1b[201~").len(), 4);
    }
}
