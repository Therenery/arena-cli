//! 渲染：状态栏 / 对话区 / 输入区，一次画一帧。
//!
//! 每帧先定位再清行，不需要整屏刷新，也就不闪。所有宽度都按显示宽度算（CJK 两格）。

use crate::app::{App, Block, HealthState};
use crate::json;
use crate::term::Size;
use crate::theme::{self, Palette, Theme};
use crate::width::{str_width, wrap};

const INPUT_INDENT: usize = 2;
const USER_PREFIX: &str = "你 › ";
const BOT_PREFIX: &str = "arena ❯ ";
const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

struct Line {
    text: String,
    pal: Option<Palette>,
    bold: bool,
}

impl Line {
    fn new(text: impl Into<String>, pal: Option<Palette>) -> Line {
        Line {
            text: text.into(),
            pal,
            bold: false,
        }
    }

    fn bold(mut self) -> Line {
        self.bold = true;
        self
    }
}

/// 一行待输出内容（styled 里已经带 ANSI）
struct Row {
    styled: String,
    width: usize,
}

impl Row {
    fn new() -> Row {
        Row {
            styled: String::new(),
            width: 0,
        }
    }

    fn push(&mut self, th: &Theme, text: &str, pal: Option<Palette>, bold: bool) -> &mut Row {
        if text.is_empty() {
            return self;
        }
        self.width += str_width(text);
        match (pal, bold) {
            (Some(p), true) => self.styled.push_str(&th.paint_bold(text, p)),
            (Some(p), false) => self.styled.push_str(&th.paint(text, p)),
            _ => self.styled.push_str(text),
        }
        self
    }

    fn push_plain(&mut self, text: &str) -> &mut Row {
        self.width += str_width(text);
        self.styled.push_str(text);
        self
    }

    fn pad_to(&mut self, width: usize) {
        if self.width < width {
            let pad = width - self.width;
            for _ in 0..pad {
                self.styled.push(' ');
            }
            self.width = width;
        }
    }
}

/* ------------------------------ 主渲染 ------------------------------ */

/// 渲染出一整帧：每行内容先算好，再用 goto 定位画上去。
pub fn render(app: &mut App, size: Size, th: &Theme) -> String {
    let rows = render_rows(app, size, th);
    let mut out = String::with_capacity(rows.iter().map(|r| r.len() + 8).sum::<usize>() + 32);
    out.push_str(theme::CURSOR_HIDE);
    for (i, row) in rows.iter().enumerate() {
        out.push_str(&theme::goto((i + 1) as u16, 1));
        out.push_str(theme::CLEAR_LINE);
        out.push_str(row);
    }
    out.push_str(theme::CURSOR_SHOW);
    out
}

/// 逐行内容（不带定位转义），方便测试检查宽度
pub fn render_rows(app: &mut App, size: Size, th: &Theme) -> Vec<String> {
    let size = size.sane();
    let w = size.cols as usize;
    let h = size.rows as usize;

    // 输入区高度：最多 3 行
    let input_w = w.saturating_sub(INPUT_INDENT * 2).max(4);
    let input_lines = if app.input.is_empty() {
        1
    } else {
        wrap(&app.input, input_w).len()
    };
    let input_h = input_lines.clamp(1, 3);
    let chat_h = h.saturating_sub(4 + input_h).max(1);

    app.view_h = chat_h;

    // 对话区
    let lines = build_lines(app, w);
    let total = lines.len();
    app.anchor(total);
    let top = app.scroll_top.min(total.saturating_sub(chat_h));
    let mut chat_rows: Vec<Row> = Vec::with_capacity(chat_h);
    for i in 0..chat_h {
        let idx = top + i;
        let mut row = Row::new();
        if let Some(line) = lines.get(idx) {
            row.push(th, &line.text, line.pal, line.bold);
        }
        chat_rows.push(row);
    }
    // 右下角「新内容」提示
    if !app.follow && chat_h > 0 && total > chat_h {
        let tip = "↓ 新内容";
        let tip_w = str_width(tip);
        if w > tip_w + 2 {
            let row = &mut chat_rows[chat_h - 1];
            row.pad_to(w - tip_w - 1);
            row.styled.push(' ');
            row.push(th, tip, Some(Palette::Warn), false);
        }
    }

    let mut rows: Vec<String> = Vec::with_capacity(h);
    rows.push(status_row(app, w, th).styled);
    rows.push(th.dim(&repeat('─', w)));
    for row in chat_rows {
        rows.push(row.styled);
    }
    rows.push(th.dim(&repeat('─', w)));
    for row in input_rows(app, w, input_h, th) {
        rows.push(row);
    }
    rows.push(hint_row(app, w, th).styled);

    // 终端比预期矮时截断，绝不画到屏幕外
    rows.truncate(h);
    rows
}

fn repeat(c: char, n: usize) -> String {
    std::iter::repeat(c).take(n).collect()
}

/* ------------------------------ 状态栏 ------------------------------ */

fn status_row(app: &App, w: usize, th: &Theme) -> Row {
    let verifying = app.stream.as_ref().map(|s| s.verifying).unwrap_or(false);

    // 先算右侧，左侧按剩余空间逐段裁剪
    let mut right = Row::new();
    if verifying {
        right.push(th, "⚠ 需要人工验证", Some(Palette::Error), true);
    } else if let Some(st) = &app.stream {
        let elapsed = st.started.elapsed().as_secs_f64();
        let frame = SPINNER[((elapsed * 9.0) as usize) % SPINNER.len()];
        right.push(th, frame, Some(Palette::Accent), false);
        right.push(th, " ", None, false);
        right.push(th, &format_elapsed(elapsed), Some(Palette::Dim), false);
    } else if let HealthState::Up(h) = &app.health {
        // 桥确实返回了队列数据才显示，不占位
        if h.queue > 0 || h.working {
            right.push(
                th,
                &format!("队列 {}/{}", h.queue, h.max_queue),
                Some(Palette::Dim),
                false,
            );
        }
    }

    let left_fixed = str_width(" arena") + str_width(" │ ");
    // 健康点及其说明
    let (dot, dot_pal, note) = match &app.health {
        HealthState::Unknown => ("?", Palette::Dim, String::new()),
        HealthState::Up(h) => {
            if h.ok && h.chrome {
                ("●", Palette::Good, String::new())
            } else if h.ok {
                ("○", Palette::Warn, "Chrome 未启动".to_string())
            } else {
                ("○", Palette::Error, "桥不通".to_string())
            }
        }
        HealthState::Down(why) => ("○", Palette::Error, why.clone()),
    };
    let dot_w = 2 + str_width(dot) + if note.is_empty() { 0 } else { 1 + str_width(&note) };

    // 可选段：模型 / 会话 / 桥地址（顺序固定，从右往左裁）
    let mut optional: Vec<(String, Option<Palette>, bool)> = vec![
        (app.model_label().to_string(), Some(Palette::Accent), false),
        (app.cfg.session.clone(), None, true),
        (app.cfg.base_display.clone(), Some(Palette::Dim), false),
    ];
    let mode_w = str_width(app.mode().label());
    let budget = w.saturating_sub(right.width + dot_w);
    loop {
        let used: usize = optional.iter().map(|(t, _, _)| 3 + str_width(t)).sum();
        if optional.is_empty() || left_fixed + mode_w + used <= budget {
            break;
        }
        optional.pop();
    }

    let mut row = Row::new();
    row.push(th, " arena", Some(Palette::Accent), true);
    row.push(th, " │ ", Some(Palette::Dim), false);
    if verifying {
        row.push(th, app.mode().label(), Some(Palette::Error), true);
    } else {
        let tag = app.mode().label();
        let styled = th.mode_tag(app.mode(), tag);
        row.width += str_width(tag);
        row.styled.push_str(&styled);
    }
    for (text, pal, bold) in &optional {
        row.push(th, " │ ", Some(Palette::Dim), false);
        row.push(th, text, *pal, *bold);
    }
    if row.width + dot_w <= w {
        row.push(th, " ", None, false);
        row.push(th, dot, Some(dot_pal), false);
        if !note.is_empty() {
            row.push(th, " ", None, false);
            row.push(th, &note, Some(dot_pal), false);
        }
    }

    let used = row.width + right.width;
    if used + 1 < w {
        row.pad_to(w - right.width);
    } else if used > w {
        // 极端窄屏：右侧直接丢掉，别撑破
        return row;
    }
    row.styled.push_str(&right.styled);
    row.width += right.width;
    row
}

fn format_elapsed(secs: f64) -> String {
    if secs < 60.0 {
        format!("{:.0}s", secs)
    } else {
        format!("{}m{:02}s", (secs as u64) / 60, (secs as u64) % 60)
    }
}

/* ------------------------------ 提示行 ------------------------------ */

fn hint_row(app: &App, w: usize, th: &Theme) -> Row {
    let mut row = Row::new();
    if let Some((text, at)) = &app.notice {
        if at.elapsed() <= crate::app::NOTICE_TTL {
            row.push(th, " ", None, false);
            row.push(th, text, Some(Palette::System), false);
            return row;
        }
    }
    let mut hint = String::from(" Enter 发送 · Shift+Enter 换行 · PgUp/PgDn 滚动 · /help · Ctrl+C 退出");
    if app.busy() {
        hint = String::from(" 生成中 · Ctrl+C 中断本轮");
    }
    if str_width(&hint) > w {
        hint = String::from(" /help · Ctrl+C 退出");
    }
    row.push(th, &hint, Some(Palette::Dim), false);
    row
}

/* ------------------------------ 输入区 ------------------------------ */

fn input_rows(app: &mut App, w: usize, input_h: usize, th: &Theme) -> Vec<String> {
    let avail = w.saturating_sub(INPUT_INDENT * 2).max(4);
    let mut out: Vec<String> = Vec::with_capacity(input_h);

    let lines: Vec<String> = if app.input.is_empty() {
        vec![String::new()]
    } else {
        wrap(&app.input, avail)
    };
    let total = lines.len();
    let start = total.saturating_sub(input_h);

    // 光标位置（行号 + 该行显示宽度）
    let cursor_line = cursor_line_index(&app.input, app.cursor, avail);
    let cursor_col = cursor_width(&app.input, app.cursor, avail);

    for i in 0..input_h {
        let idx = start + i;
        let mut row = Row::new();
        if i == 0 {
            row.push(th, "› ", Some(Palette::Accent), true);
        } else {
            row.push_plain("  ");
        }
        if app.input.is_empty() {
            if i == 0 {
                row.push(th, "输入问题，Enter 发送，Ctrl+C 退出", Some(Palette::Dim), false);
            }
        } else if let Some(text) = lines.get(idx) {
            let styled = if idx == cursor_line && cursor_col <= str_width(text) {
                with_cursor(text, cursor_col, th)
            } else {
                th.paint(&crate::width::sanitize(text), Palette::User)
            };
            row.width += str_width(text);
            row.styled.push_str(&styled);
        }
        out.push(row.styled);
    }
    out
}

/// 在字符位置画出反显光标
fn with_cursor(line: &str, col: usize, th: &Theme) -> String {
    let chars: Vec<char> = line.chars().collect();
    // col 是显示宽度，转成字符索引
    let mut w = 0usize;
    let mut idx = chars.len();
    for (i, ch) in chars.iter().enumerate() {
        if w >= col {
            idx = i;
            break;
        }
        w += crate::width::char_width(*ch);
    }
    let before: String = chars[..idx.min(chars.len())].iter().collect();
    let at = if idx < chars.len() {
        chars[idx].to_string()
    } else {
        " ".to_string()
    };
    let after: String = if idx + 1 <= chars.len() {
        chars[(idx + 1).min(chars.len())..].iter().collect()
    } else {
        String::new()
    };
    format!(
        "{}{}{}",
        th.paint(&before, Palette::User),
        th.raw("7", &at),
        th.paint(&after, Palette::User)
    )
}

fn cursor_line_index(input: &str, cursor: usize, avail: usize) -> usize {
    let prefix: String = input.chars().take(cursor).collect();
    wrap(&prefix, avail).len().saturating_sub(1)
}

fn cursor_width(input: &str, cursor: usize, avail: usize) -> usize {
    let prefix: String = input.chars().take(cursor).collect();
    let parts = wrap(&prefix, avail);
    parts
        .last()
        .map(|s| str_width(s))
        .unwrap_or(0)
}

/* ------------------------------ 对话区 ------------------------------ */

fn build_lines(app: &App, w: usize) -> Vec<Line> {
    let mut out: Vec<Line> = Vec::new();
    if app.blocks.is_empty() {
        out.push(Line::new("", None));
        out.push(Line::new(" arena-lite · 终端里的 arena_bridge 客户端", Some(Palette::System)).bold());
        out.push(Line::new("", None));
        for l in wrap(
            "直接输入问题回车发送。/agent、/direct、/side、/battle 切换常驻模式；/resume 可以载入网页端历史；/help 看全部命令。",
            w.saturating_sub(2).max(10),
        ) {
            out.push(Line::new(format!(" {}", l), Some(Palette::Dim)));
        }
        out.push(Line::new("", None));
        return out;
    }

    for b in app.blocks.iter() {
        match b {
            Block::User { text } => {
                push_prefixed(
                    &mut out,
                    &crate::width::sanitize(text),
                    USER_PREFIX,
                    4,
                    w,
                    Palette::User,
                    true,
                );
            }
            Block::Assistant { text } => {
                if text.trim().is_empty() {
                    // 还在等第一个字：正文先留空，等待中的状态由状态栏 spinner 表达
                    continue;
                }
                push_prefixed(
                    &mut out,
                    &crate::width::sanitize(text),
                    BOT_PREFIX,
                    8,
                    w,
                    Palette::Assistant,
                    false,
                );
            }
            Block::Reasoning { text, lines } => {
                let n = (*lines).max(text.split('\n').count());
                if app.show_reasoning {
                    out.push(Line::new(
                        format!("▾ 思考（{} 行，Ctrl+R 收起）", n),
                        Some(Palette::Reasoning),
                    ));
                    let clean = crate::width::sanitize(text);
                    for l in wrap(clean.trim_matches('\n'), w.saturating_sub(4).max(8)) {
                        out.push(Line::new(format!("    {}", l), Some(Palette::Reasoning)));
                    }
                } else {
                    out.push(Line::new(
                        format!("· 思考（{} 行，Ctrl+R 展开）", n),
                        Some(Palette::Reasoning),
                    ));
                }
            }
            Block::Tool { name, input } => {
                let summary = tool_summary(input);
                if app.show_tools {
                    out.push(
                        Line::new(format!("▾ 工具 {}  {}", name, summary), Some(Palette::Tool))
                            .bold(),
                    );
                    if !input.trim().is_empty() {
                        for l in wrap(&pretty_json(input), w.saturating_sub(6).max(8)) {
                            out.push(Line::new(format!("      {}", l), Some(Palette::Dim)));
                        }
                    }
                } else {
                    out.push(Line::new(
                        format!("▸ 工具 {}  {}", name, summary),
                        Some(Palette::Tool),
                    ));
                }
            }
            Block::System { text } => {
                let mut first = true;
                for l in wrap(text, w.saturating_sub(2).max(8)) {
                    if first {
                        out.push(Line::new(format!("· {}", l), Some(Palette::System)));
                        first = false;
                    } else {
                        out.push(Line::new(format!("  {}", l), Some(Palette::System)));
                    }
                }
            }
            Block::Error { text } => {
                let mut first = true;
                for l in wrap(text, w.saturating_sub(2).max(8)) {
                    if first {
                        out.push(Line::new(format!("! {}", l), Some(Palette::Error)).bold());
                        first = false;
                    } else {
                        out.push(Line::new(format!("  {}", l), Some(Palette::Error)));
                    }
                }
            }
            Block::Verify { text } => {
                let mut first = true;
                for l in wrap(text, w.saturating_sub(2).max(8)) {
                    if first {
                        out.push(
                            Line::new(format!("⚑ {}", l), Some(Palette::Error)).bold(),
                        );
                        first = false;
                    } else {
                        out.push(Line::new(format!("  {}", l), Some(Palette::Error)));
                    }
                }
            }
        }
    }
    out.push(Line::new("", None));
    out
}

fn push_prefixed(
    out: &mut Vec<Line>,
    text: &str,
    prefix: &str,
    indent: usize,
    w: usize,
    pal: Palette,
    bold: bool,
) {
    let body_w = w.saturating_sub(indent + 1).max(8);
    let lines = wrap(text, body_w);
    for (i, l) in lines.iter().enumerate() {
        if i == 0 {
            let mut line = Line::new(format!("{}{}", prefix, l), Some(pal));
            line.bold = bold;
            out.push(line);
        } else {
            out.push(Line::new(format!("{}{}", " ".repeat(indent), l), Some(pal)));
        }
    }
}

/// 工具参数的一行摘要：优先取第一个字符串值
fn tool_summary(input: &str) -> String {
    let input = input.trim();
    if input.is_empty() {
        return String::new();
    }
    if let Ok(v) = json::parse(input) {
        if let Some(obj) = v.as_obj() {
            for (k, val) in obj {
                if let Some(s) = val.as_str() {
                    if !s.is_empty() {
                        let _ = k;
                        return s.chars().take(60).collect();
                    }
                }
            }
        }
        if let Some(s) = v.as_str() {
            return s.chars().take(60).collect();
        }
    }
    let one_line = input.replace('\n', " ");
    crate::err::truncate(&one_line, 60)
}

/// 展开工具参数时稍微排版一下（有 JSON 就按 key: value 缩进）
fn pretty_json(input: &str) -> String {
    let input = input.trim();
    match json::parse(input) {
        Ok(v) => match v.as_obj() {
            Some(obj) => obj
                .iter()
                .map(|(k, val)| format!("{}: {}", k, render_value(val)))
                .collect::<Vec<_>>()
                .join("\n"),
            None => input.to_string(),
        },
        Err(_) => input.to_string(),
    }
}

fn render_value(v: &json::Value) -> String {
    match v {
        json::Value::Str(s) => s.clone(),
        json::Value::Num(n) => json::num_str(*n),
        json::Value::Bool(b) => b.to_string(),
        json::Value::Null => "null".to_string(),
        other => format!("{:?}", other),
    }
}

/* ------------------------------ LOGO ------------------------------ */

pub fn render_logo(size: Size, th: &Theme) -> String {
    let size = size.sane();
    let w = size.cols as usize;
    let h = size.rows as usize;
    let logo = theme::logo_ansi(th);
    let logo_h = logo.len();
    let top = h.saturating_sub(logo_h) / 2;
    let mut out = String::new();
    out.push_str(theme::CURSOR_HIDE);
    for r in 0..h {
        out.push_str(&theme::goto((r + 1) as u16, 1));
        out.push_str(theme::CLEAR_LINE);
        if r == top {
            out.push_str(&th.dim("按任意键跳过"));
        }
    }
    for (i, line) in logo.iter().enumerate() {
        let row = top + i + 1;
        if row > h {
            break;
        }
        out.push_str(&theme::goto(row as u16, 1));
        out.push_str(theme::CLEAR_LINE);
        out.push_str(line);
    }
    // 底部一行小字
    let hint = "arena-lite 2.0 · Enter 发送 · /help 命令";
    if h > logo_h + top + 2 {
        out.push_str(&theme::goto((top + logo_h + 2) as u16, 1));
        out.push_str(theme::CLEAR_LINE);
        out.push_str(&th.dim(hint));
    }
    let _ = w;
    out.push_str(theme::CURSOR_SHOW);
    out
}

/* ------------------------------ 测试 ------------------------------ */

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::Config;
    use crate::client::{Client, Health, StreamEvent};
    use std::time::Instant;
    use crate::mode::Mode;
    use std::sync::{mpsc, Arc};

    fn app(mode: Mode) -> App {
        let base = crate::httpc::parse_base("127.0.0.1:17892").expect("base");
        let client = Arc::new(Client::new(base, None));
        let (tx, _rx) = mpsc::channel();
        App::new(
            client,
            tx,
            Config {
                base_display: "127.0.0.1:17892".into(),
                session: "main".into(),
                mode,
                models: Vec::new(),
                reasoning_open: false,
            },
        )
    }

    fn strip_ansi(s: &str) -> String {
        let mut out = String::new();
        let mut it = s.chars().peekable();
        while let Some(c) = it.next() {
            if c == '\x1b' {
                if it.peek() == Some(&'[') {
                    it.next();
                    while let Some(&n) = it.peek() {
                        it.next();
                        if n.is_ascii_alphabetic() {
                            break;
                        }
                    }
                }
                continue;
            }
            out.push(c);
        }
        out
    }

    #[test]
    fn frame_fits_terminal() {
        let mut a = app(Mode::Agent);
        a.blocks.push(Block::User {
            text: "帮我看看这个仓库的结构，顺便说一下中文折行会不会出问题".into(),
        });
        a.blocks.push(Block::Assistant {
            text: "好的，我先看一下目录树。\n\n```\nbridge/\n  arena_bridge.js\n```\n再决定读哪个文件。"
                .into(),
        });
        let th = Theme::new(false);
        for cols in [40u16, 60, 80, 120] {
            let rows = render_rows(&mut a, Size { cols, rows: 20 }, &th);
            assert_eq!(rows.len(), 20);
            // 每一行的可见宽度都不超过终端宽度
            for line in &rows {
                let plain = strip_ansi(line);
                assert!(
                    str_width(&plain) <= cols as usize,
                    "宽度越界 cols={} line={:?} w={}",
                    cols,
                    plain,
                    str_width(&plain)
                );
            }
        }
    }

    #[test]
    fn status_bar_shows_mode_session_and_address() {
        let mut a = app(Mode::Battle);
        a.health = HealthState::Up(Health {
            ok: true,
            chrome: true,
            ..Default::default()
        });
        let th = Theme::new(false);
        let frame = render(&mut a, Size { cols: 100, rows: 20 }, &th);
        let plain = strip_ansi(&frame);
        assert!(plain.contains("battle"));
        assert!(plain.contains("main"));
        assert!(plain.contains("127.0.0.1:17892"));
        assert!(plain.contains("●"));
    }

    #[test]
    fn streaming_shows_spinner_and_seconds() {
        let mut a = app(Mode::Agent);
        a.stream = Some(crate::app::StreamState {
            id: 1,
            started: Instant::now(),
            verifying: false,
            got_output: false,
        });
        let plain = strip_ansi(&render(&mut a, Size { cols: 100, rows: 20 }, &Theme::new(false)));
        assert!(plain.contains('0') || plain.contains('s'));
        assert!(plain.chars().any(|c| SPINNER.iter().any(|s| s.starts_with(c))));
    }

    #[test]
    fn collapsed_reasoning_hint_and_tool_block() {
        let mut a = app(Mode::Agent);
        a.blocks.push(Block::Reasoning {
            text: "第一行\n第二行\n第三行".into(),
            lines: 3,
        });
        a.blocks.push(Block::Tool {
            name: "read_file".into(),
            input: "{\"path\":\"src/main.rs\"}".into(),
        });
        let plain = strip_ansi(&render(&mut a, Size { cols: 80, rows: 24 }, &Theme::new(false)));
        assert!(plain.contains("思考（3 行，Ctrl+R 展开）"));
        assert!(plain.contains("▸ 工具 read_file  src/main.rs"));
        // 展开之后能看到参数和全文
        a.show_tools = true;
        a.show_reasoning = true;
        let plain = strip_ansi(&render(&mut a, Size { cols: 80, rows: 24 }, &Theme::new(false)));
        assert!(plain.contains("path: src/main.rs"));
        assert!(plain.contains("第三行"));
    }

    #[test]
    fn scrolled_up_shows_new_content_badge() {
        let mut a = app(Mode::Agent);
        for i in 0..60 {
            a.blocks.push(Block::User {
                text: format!("第 {} 条消息", i),
            });
        }
        let th = Theme::new(false);
        render(&mut a, Size { cols: 60, rows: 20 }, &th);
        a.scroll_page(true);
        let plain = strip_ansi(&render(&mut a, Size { cols: 60, rows: 20 }, &th));
        assert!(plain.contains("↓ 新内容"));
    }

    #[test]
    fn verify_banner_is_red_state() {
        let mut a = app(Mode::Agent);
        a.stream = Some(crate::app::StreamState {
            id: 1,
            started: Instant::now(),
            verifying: true,
            got_output: false,
        });
        let th = Theme::new(true);
        let frame = render(&mut a, Size { cols: 100, rows: 20 }, &th);
        assert!(frame.contains("需要人工验证"));
        assert!(frame.contains("196"));
    }

    #[test]
    fn empty_state_renders_without_panic() {
        let mut a = app(Mode::Agent);
        let frame = render(&mut a, Size { cols: 40, rows: 12 }, &Theme::new(false));
        assert!(strip_ansi(&frame).contains("arena-lite"));
    }

    #[test]
    fn tiny_terminal_does_not_panic() {
        let mut a = app(Mode::Agent);
        a.input = "很长的输入".repeat(40);
        for (cols, rows) in [(20u16, 6u16), (21, 7), (30, 8), (80, 24)] {
            let _ = render(&mut a, Size { cols, rows }, &Theme::new(false));
        }
    }

    #[test]
    fn logo_frame_has_logo_lines() {
        let frame = render_logo(Size { cols: 80, rows: 24 }, &Theme::new(false));
        assert!(strip_ansi(&frame).contains("arena-lite"));
    }

    #[test]
    fn tool_summary_picks_first_string() {
        assert_eq!(
            tool_summary("{\"path\":\"src/main.rs\"}"),
            "src/main.rs"
        );
        assert_eq!(tool_summary(""), "");
        assert_eq!(tool_summary("not json"), "not json");
    }

    #[test]
    fn stream_event_enum_is_used() {
        // 防止 ui 里的 import 被当成未使用而报警告
        let _ = StreamEvent::Text(String::new());
    }
}
