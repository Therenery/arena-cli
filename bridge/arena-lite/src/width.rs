//! 终端显示宽度与折行：CJK 算两格，组合符算零格。
//!
//! 终端里中英混排是最容易出问题的地方（上一版表格被中文撑歪过），
//! 所以宽度计算和折行都做成纯函数，单独测。

/// 单个字符的显示宽度（0/1/2 格）
pub fn char_width(c: char) -> usize {
    let cp = c as u32;

    // 零宽：组合符、变体选择符、零宽连接/空格
    if cp < 0x20 {
        return 0;
    }
    if cp == 0x7f {
        return 0;
    }
    if (0x0300..=0x036F).contains(&cp)      // 组合变音符
        || (0x0483..=0x0489).contains(&cp)
        || (0x0591..=0x05BD).contains(&cp)
        || (0x0610..=0x061A).contains(&cp)
        || (0x064B..=0x065F).contains(&cp)
        || (0x0E31..=0x0E3A).contains(&cp)
        || (0x1AB0..=0x1AFF).contains(&cp)
        || (0x1DC0..=0x1DFF).contains(&cp)
        || (0x200B..=0x200F).contains(&cp)  // 零宽空格/方向标记
        || (0x20D0..=0x20F0).contains(&cp)
        || (0xFE00..=0xFE0F).contains(&cp)  // 变体选择符
        || (0xFE20..=0xFE2F).contains(&cp)
        || cp == 0xFEFF
        || (0xE0100..=0xE01EF).contains(&cp)
    {
        return 0;
    }

    // 双宽：CJK、假名、谚文、全角标点、以及常见宽 emoji
    if (0x1100..=0x115F).contains(&cp)
        || (0x2E80..=0x303E).contains(&cp)   // CJK 部首、标点
        || (0x3041..=0x33FF).contains(&cp)   // 假名、CJK 兼容
        || (0x3400..=0x4DBF).contains(&cp)   // CJK 扩展 A
        || (0x4E00..=0x9FFF).contains(&cp)   // CJK 统一表意
        || (0xA000..=0xA4CF).contains(&cp)   // 彝文
        || (0xA960..=0xA97F).contains(&cp)
        || (0xAC00..=0xD7A3).contains(&cp)   // 谚文音节
        || (0xF900..=0xFAFF).contains(&cp)   // CJK 兼容表意
        || (0xFE10..=0xFE19).contains(&cp)   // 竖排标点
        || (0xFE30..=0xFE6F).contains(&cp)   // CJK 兼容形式
        || (0xFF00..=0xFF60).contains(&cp)   // 全角 ASCII
        || (0xFFE0..=0xFFE6).contains(&cp)   // 全角符号
        || (0x1F300..=0x1F64F).contains(&cp) // 图形 emoji
        || (0x1F900..=0x1F9FF).contains(&cp)
        || (0x1FA70..=0x1FAFF).contains(&cp)
        || (0x17000..=0x187F7).contains(&cp)
        || (0x20000..=0x3FFFD).contains(&cp) // CJK 扩展 B 及以上
    {
        return 2;
    }

    1
}

/// 字符串的显示宽度
pub fn str_width(s: &str) -> usize {
    s.chars().map(char_width).sum()
}

/// 逻辑行折行：CJK 可以在任意两字之间断，拉丁文优先在空格/标点后断。
fn wrap_one(line: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut cur_w = 0usize;
    // 最近一个「可以在它后面断」的位置（字节索引 + 当时的宽度）
    let mut brk: Option<(usize, usize)> = None;

    for ch in line.chars() {
        let cw = char_width(ch);
        if cw == 0 {
            cur.push(ch);
            continue;
        }
        if cur_w + cw > width && !cur.is_empty() {
            match brk {
                Some((bpos, bw)) if bpos > 0 && bpos <= cur.len() => {
                    let head = cur[..bpos].trim_end().to_string();
                    let tail = cur[bpos..].to_string();
                    out.push(head);
                    cur = tail;
                    cur_w -= bw;
                    // 折完之后重新算一次，避免残留仍超宽
                    while cur_w + cw > width && !cur.is_empty() {
                        let take = take_prefix(&cur, width);
                        if take.0 == 0 {
                            cur.clear();
                            cur_w = 0;
                            break;
                        }
                        out.push(take.1);
                        cur = take.2;
                        cur_w -= take.0;
                    }
                }
                _ => {
                    out.push(std::mem::take(&mut cur));
                    cur_w = 0;
                }
            }
            brk = None;
        }
        cur.push(ch);
        cur_w += cw;
        if is_break_after(ch) {
            brk = Some((cur.len(), cur_w));
        }
    }
    if !cur.is_empty() || out.is_empty() {
        out.push(cur.trim_end().to_string());
    }
    out
}

/// 从头部取出宽度不超过 `width` 的一段，返回（取走的宽度，取走的文本，剩下的文本）
fn take_prefix(s: &str, width: usize) -> (usize, String, String) {
    let mut w = 0usize;
    let mut idx = 0usize;
    for (i, ch) in s.char_indices() {
        let cw = char_width(ch);
        if w + cw > width {
            idx = i;
            break;
        }
        w += cw;
        idx = i + ch.len_utf8();
    }
    (w, s[..idx].to_string(), s[idx..].to_string())
}

/// 这个字符后面可以断行吗
fn is_break_after(c: char) -> bool {
    if c == ' ' || c == '\t' {
        return true;
    }
    matches!(
        c,
        '，' | '。'
            | '、'
            | '；'
            | '：'
            | '？'
            | '！'
            | '）'
            | '》'
            | '」'
            | '』'
            | '】'
            | '…'
            | ','
            | '.'
            | ';'
            | ':'
            | '?'
            | '!'
            | ')'
            | ']'
            | '}'
            | '>'
            | '/'
            | '-'
    )
}

/// 多行文本折行：先按 `\n` 切逻辑行，再逐行折
pub fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut out: Vec<String> = Vec::new();
    for line in text.split('\n') {
        // tab 展开成 4 空格；\r 丢掉，免得 Windows 文本带出怪字符
        let line = line.replace('\t', "    ").replace('\r', "");
        let mut part = wrap_one(&line, width);
        if part.is_empty() {
            part.push(String::new());
        }
        out.extend(part);
    }
    if out.is_empty() {
        out.push(String::new());
    }
    out
}

/// 终端控制字符不打印，统一替换成可见占位
pub fn sanitize(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '\n' | '\t' => out.push(ch),
            '\r' => {}
            c if (c as u32) < 0x20 || c as u32 == 0x7f => out.push(' '),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_is_one_wide() {
        assert_eq!(char_width('a'), 1);
        assert_eq!(char_width(' '), 1);
        assert_eq!(str_width("hello"), 5);
    }

    #[test]
    fn cjk_is_two_wide() {
        assert_eq!(char_width('你'), 2);
        assert_eq!(char_width('好'), 2);
        assert_eq!(char_width('ア'), 2);
        assert_eq!(char_width('한'), 2);
        assert_eq!(str_width("你好"), 4);
        assert_eq!(str_width("a你b"), 4);
        assert_eq!(char_width('，'), 2);
        assert_eq!(char_width('😀'), 2);
    }

    #[test]
    fn combining_marks_are_zero_wide() {
        // e + U+0301（组合尖音符）
        assert_eq!(str_width("e\u{301}"), 1);
        assert_eq!(char_width('\u{200b}'), 0);
    }

    #[test]
    fn wrap_ascii_at_spaces() {
        let got = wrap("the quick brown fox", 10);
        assert_eq!(got, vec!["the quick", "brown fox"]);
    }

    #[test]
    fn wrap_cjk_by_display_width() {
        let got = wrap("你好世界", 4);
        assert_eq!(got, vec!["你好", "世界"]);
    }

    #[test]
    fn wrap_never_splits_wide_char() {
        // 宽度 3：一个汉字两格，不能再塞第二个汉字，留着空格补位
        let got = wrap("你好世界", 3);
        assert_eq!(got, vec!["你", "好", "世", "界"]);
    }

    #[test]
    fn wrap_mixed_cjk_latin() {
        let got = wrap("请读一下 src/main.rs", 8);
        assert!(got.len() >= 2);
        for line in &got {
            assert!(str_width(line) <= 8, "越界: {:?} 宽 {}", line, str_width(line));
        }
        // 拼接后内容不丢（空格可能因折行被吃掉）
        let joined: String = got.join("");
        let compact: String = joined.chars().filter(|c| !c.is_whitespace()).collect();
        assert!(compact.contains("src/main.rs"));
    }

    #[test]
    fn wrap_long_word_forced() {
        let got = wrap("abcdefghijklmnop", 5);
        assert_eq!(got, vec!["abcde", "fghij", "klmno", "p"]);
    }

    #[test]
    fn wrap_keeps_blank_lines() {
        let got = wrap("a\n\nb", 10);
        assert_eq!(got, vec!["a", "", "b"]);
    }

    #[test]
    fn wrap_tabs_and_crlf() {
        let got = wrap("a\tb\r\nc", 20);
        assert_eq!(got, vec!["a    b", "c"]);
    }

    #[test]
    fn all_lines_fit() {
        let text = "这是一个很长的中文句子，用来检查折行会不会越界。mixed english words here.";
        for w in 4..40 {
            for line in wrap(text, w) {
                assert!(
                    str_width(&line) <= w,
                    "width={} line={:?} actual={}",
                    w,
                    line,
                    str_width(&line)
                );
            }
        }
    }

    #[test]
    fn sanitize_strips_controls() {
        assert_eq!(sanitize("a\u{7}b"), "a b");
        assert_eq!(sanitize("a\rb"), "ab");
    }
}
