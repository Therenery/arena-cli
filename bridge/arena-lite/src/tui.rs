//! 简洁 TUI：顶部状态栏 + 可滚动对话区 + 底部输入行。
//!
//! 零依赖实现：raw mode 用 `stty` 开关，渲染用 ANSI 转义，
//! 键盘事件由一个线程读 stdin 后解析，网络流由发起线程解析 SSE，
//! 主循环用一个 channel 收两类事件。

use crate::bridge::{fmt_health, Bridge, ChatRequest, StreamEvent};
use crate::Config;
use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::process::Command;
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant};

const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

const HELP: &str = "模式(常驻, 随时切换, 像网页端一样): /agent /direct /side /battle [问题...] /mode <agent|direct|battle|side>\n对战栏模型: /panemodels <a,b>   投票: /vote <a|b|tie|both>\n命令: /model [id] /session [名] /new /models /health /repos /repo <owner/name>\n      /reasoning [on|off] /upload <文件...> /clear /quit\n按键: Enter 发送 · Ctrl+C 退出 · Esc 清空 · ↑↓ 历史 · PgUp/PgDn 滚动";

// ---------- 终端 ----------

struct Term {
    saved: Option<String>,
}

impl Term {
    fn new() -> io::Result<Term> {
        let saved = Command::new("stty")
            .arg("-g")
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
        let st = Command::new("stty").args(["raw", "-echo"]).status()?;
        if !st.success() {
            return Err(io::Error::new(io::ErrorKind::Other, "stty raw 失败"));
        }
        let mut out = io::stdout();
        out.write_all(b"\x1b[?1049h\x1b[?25l\x1b[2J\x1b[H")?;
        out.flush()?;
        Ok(Term { saved })
    }

    fn size() -> (usize, usize) {
        if let Ok(o) = Command::new("stty").arg("size").output() {
            let s = String::from_utf8_lossy(&o.stdout);
            let mut it = s.split_whitespace();
            if let (Some(r), Some(c)) = (it.next(), it.next()) {
                if let (Ok(r), Ok(c)) = (r.parse::<usize>(), c.parse::<usize>()) {
                    return (r.max(8), c.max(20));
                }
            }
        }
        (24, 80)
    }

    fn is_tty() -> bool {
        Command::new("stty")
            .arg("size")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }
}

impl Drop for Term {
    fn drop(&mut self) {
        let mut out = io::stdout();
        let _ = out.write_all(b"\x1b[0m\x1b[?25h\x1b[?1049l");
        let _ = out.flush();
        if let Some(s) = &self.saved {
            let _ = Command::new("stty").arg(s).status();
        } else {
            let _ = Command::new("stty").arg("sane").status();
        }
    }
}

// ---------- 按键 ----------

#[derive(Debug, Clone, Copy, PartialEq)]
enum Key {
    Char(char),
    Enter,
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
    Esc,
    CtrlC,
    CtrlD,
    CtrlU,
    CtrlW,
    CtrlA,
    CtrlE,
    Ignore,
}

fn utf8_len(b: u8) -> usize {
    if b < 0x80 {
        1
    } else if b >> 5 == 0b110 {
        2
    } else if b >> 4 == 0b1110 {
        3
    } else if b >> 3 == 0b11110 {
        4
    } else {
        1
    }
}

fn parse_keys(buf: &mut Vec<u8>) -> Vec<Key> {
    let mut keys = Vec::new();
    while !buf.is_empty() {
        let b = buf[0];
        match b {
            0x03 => {
                keys.push(Key::CtrlC);
                buf.remove(0);
            }
            0x04 => {
                keys.push(Key::CtrlD);
                buf.remove(0);
            }
            0x7f | 0x08 => {
                keys.push(Key::Backspace);
                buf.remove(0);
            }
            0x15 => {
                keys.push(Key::CtrlU);
                buf.remove(0);
            }
            0x17 => {
                keys.push(Key::CtrlW);
                buf.remove(0);
            }
            0x01 => {
                keys.push(Key::CtrlA);
                buf.remove(0);
            }
            0x05 => {
                keys.push(Key::CtrlE);
                buf.remove(0);
            }
            b'\r' | b'\n' => {
                keys.push(Key::Enter);
                buf.remove(0);
            }
            0x00 => {
                keys.push(Key::Ignore);
                buf.remove(0);
            }
            0x1b => {
                if buf.len() == 1 {
                    keys.push(Key::Esc);
                    buf.remove(0);
                } else if buf[1] == b'[' || buf[1] == b'O' {
                    if buf.len() >= 3 {
                        let k = match (buf[1], buf[2]) {
                            (b'[', b'A') | (b'O', b'A') => Some(Key::Up),
                            (b'[', b'B') | (b'O', b'B') => Some(Key::Down),
                            (b'[', b'C') | (b'O', b'C') => Some(Key::Right),
                            (b'[', b'D') | (b'O', b'D') => Some(Key::Left),
                            (b'[', b'H') | (b'O', b'H') => Some(Key::Home),
                            (b'[', b'F') | (b'O', b'F') => Some(Key::End),
                            _ => None,
                        };
                        if let Some(k) = k {
                            keys.push(k);
                            buf.drain(0..3);
                            continue;
                        }
                        if buf[1] == b'[' && buf.len() >= 4 && buf[3] == b'~' {
                            let k = match buf[2] {
                                b'3' => Some(Key::Delete),
                                b'5' => Some(Key::PageUp),
                                b'6' => Some(Key::PageDown),
                                b'1' | b'7' => Some(Key::Home),
                                b'4' | b'8' => Some(Key::End),
                                _ => None,
                            };
                            if let Some(k) = k {
                                keys.push(k);
                                buf.drain(0..4);
                                continue;
                            }
                        }
                        keys.push(Key::Ignore);
                        let n = buf.len().min(3);
                        buf.drain(0..n);
                    } else {
                        break; // 序列还没读全，等下一批字节
                    }
                } else {
                    keys.push(Key::Esc); // Alt+xx，按 Esc 处理
                    buf.remove(0);
                }
            }
            _ => {
                let len = utf8_len(b);
                if buf.len() < len {
                    break; // UTF-8 字符没读全
                }
                let chunk: Vec<u8> = buf.drain(0..len).collect();
                match std::str::from_utf8(&chunk) {
                    Ok(s) => {
                        if let Some(c) = s.chars().next() {
                            keys.push(Key::Char(c));
                        }
                    }
                    Err(_) => keys.push(Key::Ignore),
                }
            }
        }
    }
    keys
}

fn spawn_key_reader() -> Receiver<Key> {
    let (tx, rx) = channel::<Key>();
    thread::spawn(move || {
        let stdin = io::stdin();
        let mut lock = stdin.lock();
        let mut buf: Vec<u8> = Vec::new();
        let mut chunk = [0u8; 128];
        loop {
            match lock.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => {
                    buf.extend_from_slice(&chunk[..n]);
                    for k in parse_keys(&mut buf) {
                        if tx.send(k).is_err() {
                            return;
                        }
                    }
                }
                Err(_) => break,
            }
        }
    });
    rx
}

// ---------- 事件与消息 ----------

enum Ev {
    Key(Key),
    Stream(StreamEvent),
    StreamEnd,
    Job(String, bool), // 后台任务结果（文字, 是否错误）
    Health(bool),
}

#[derive(Clone, Copy, PartialEq)]
enum Role {
    User,
    Assistant,
    Reasoning,
    System,
    Error,
}

struct Msg {
    role: Role,
    text: String,
}

// ---------- 应用状态 ----------

struct App {
    bridge: Bridge,
    model: String,
    session: String,
    mode: String,
    reasoning: bool,
    fresh: bool,
    transcript: Vec<Msg>,
    input: String,
    cursor: usize, // input 里的字节偏移
    hist: Vec<String>,
    hist_idx: usize,
    outbox: VecDeque<()>, // 等待串行发送的轮次
    streaming: bool,
    stream_start: Option<Instant>,
    bridge_ok: Option<bool>,
    scroll: usize, // 向上滚动的行数，0 = 底部
    view_h: usize,
    running: bool,
}

impl App {
    fn new(cfg: &Config, bridge: Bridge) -> App {
        App {
            bridge,
            model: cfg.model.clone(),
            session: cfg.session.clone(),
            mode: cfg.mode.clone(),
            pane_models: cfg.models.clone(),
            reasoning: cfg.reasoning,
            fresh: cfg.fresh,
            transcript: Vec::new(),
            input: String::new(),
            cursor: 0,
            hist: Vec::new(),
            hist_idx: 0,
            outbox: VecDeque::new(),
            streaming: false,
            stream_start: None,
            bridge_ok: None,
            scroll: 0,
            view_h: 20,
            running: true,
        }
    }

    fn sys(&mut self, text: &str) {
        self.transcript.push(Msg {
            role: Role::System,
            text: text.to_string(),
        });
    }

    fn spawn_job<F>(tx: &Sender<Ev>, f: F)
    where
        F: FnOnce() -> (String, bool) + Send + 'static,
    {
        let tx2 = tx.clone();
        thread::spawn(move || {
            let (text, is_err) = f();
            let _ = tx2.send(Ev::Job(text, is_err));
        });
    }

    fn spawn_health(&self, tx: &Sender<Ev>) {
        let b = self.bridge.clone();
        let tx2 = tx.clone();
        thread::spawn(move || {
            let ok = b.health().is_ok();
            let _ = tx2.send(Ev::Health(ok));
        });
    }

    fn start_request(&mut self, tx: &Sender<Ev>) {
        self.outbox.pop_front();
        let messages: Vec<(String, String)> = self
            .transcript
            .iter()
            .filter(|m| matches!(m.role, Role::User | Role::Assistant))
            .map(|m| {
                (
                    if m.role == Role::User { "user" } else { "assistant" }.to_string(),
                    m.text.clone(),
                )
            })
            .collect();
        let req = ChatRequest {
            model: self.model.clone(),
            session: self.session.clone(),
            mode: self.mode.clone(),
            fresh: self.fresh,
            messages,
            models: self.pane_models.clone(),
        };
        self.fresh = false;
        self.streaming = true;
        self.stream_start = Some(Instant::now());
        self.scroll = 0;
        let b = self.bridge.clone();
        let tx2 = tx.clone();
        thread::spawn(move || {
            let tx3 = tx2.clone();
            let r = b.chat_stream(&req, move |e| {
                let _ = tx3.send(Ev::Stream(e));
            });
            if let Err(e) = r {
                let _ = tx2.send(Ev::Stream(StreamEvent::Error(e)));
            }
            let _ = tx2.send(Ev::StreamEnd);
        });
    }

    fn maybe_next(&mut self, tx: &Sender<Ev>) {
        if !self.streaming && !self.outbox.is_empty() {
            self.start_request(tx);
        }
    }

    fn on_stream(&mut self, e: StreamEvent) {
        match e {
            StreamEvent::Text(t) => match self.transcript.last_mut() {
                Some(m) if m.role == Role::Assistant => m.text.push_str(&t),
                _ => self.transcript.push(Msg {
                    role: Role::Assistant,
                    text: t,
                }),
            },
            StreamEvent::Reasoning(t) => {
                if self.reasoning {
                    match self.transcript.last_mut() {
                        Some(m) if m.role == Role::Reasoning => m.text.push_str(&t),
                        _ => self.transcript.push(Msg {
                            role: Role::Reasoning,
                            text: t,
                        }),
                    }
                }
            }
            StreamEvent::Done => {
                self.streaming = false;
            }
            StreamEvent::Error(e) => {
                self.transcript.push(Msg {
                    role: Role::Error,
                    text: e,
                });
                self.streaming = false;
                if !self.outbox.is_empty() {
                    self.outbox.clear();
                    self.sys("出错了，已丢弃队列里待发的消息");
                }
            }
        }
    }

    fn handle(&mut self, ev: Ev, tx: &Sender<Ev>) {
        match ev {
            Ev::Key(k) => self.on_key(k, tx),
            Ev::Stream(e) => self.on_stream(e),
            Ev::StreamEnd => {
                self.streaming = false;
                self.maybe_next(tx);
            }
            Ev::Job(text, is_err) => self.transcript.push(Msg {
                role: if is_err { Role::Error } else { Role::System },
                text,
            }),
            Ev::Health(ok) => self.bridge_ok = Some(ok),
        }
    }

    fn submit(&mut self, tx: &Sender<Ev>) {
        let text = self.input.trim().to_string();
        self.input.clear();
        self.cursor = 0;
        if text.is_empty() {
            return;
        }
        if text.starts_with('/') {
            self.run_slash(&text, tx);
            return;
        }
        self.send_text(text, tx);
    }

    fn send_text(&mut self, text: String, tx: &Sender<Ev>) {
        self.transcript.push(Msg {
            role: Role::User,
            text: text.clone(),
        });
        self.hist.push(text);
        self.hist_idx = self.hist.len();
        if self.streaming {
            self.outbox.push_back(());
            self.sys("⏳ 桥接全局串行，已排队，上一轮结束后自动发送");
        } else {
            self.start_request(tx);
        }
    }

    /// 切换常驻页面模式（agent / direct / battle / side），之后的提问都走这个模式。
    fn set_mode(&mut self, m: &str) {
        self.mode = m.to_string();
        self.sys(&format!(
            "已切换到 {} 模式（之后的提问都走这个模式，直到再切换）",
            m
        ));
    }

    fn on_key(&mut self, key: Key, tx: &Sender<Ev>) {
        match key {
            Key::CtrlC => self.running = false,
            Key::CtrlD => {
                if self.input.is_empty() {
                    self.running = false;
                } else {
                    self.del_at_cursor();
                }
            }
            Key::Enter => self.submit(tx),
            Key::Backspace => {
                if self.cursor > 0 {
                    let prev = self.input[..self.cursor]
                        .char_indices()
                        .last()
                        .map(|(i, _)| i)
                        .unwrap_or(0);
                    self.input.replace_range(prev..self.cursor, "");
                    self.cursor = prev;
                }
            }
            Key::Delete => self.del_at_cursor(),
            Key::Left => {
                if self.cursor > 0 {
                    self.cursor = self.input[..self.cursor]
                        .char_indices()
                        .last()
                        .map(|(i, _)| i)
                        .unwrap_or(0);
                }
            }
            Key::Right => {
                if self.cursor < self.input.len() {
                    self.cursor += self.input[self.cursor..]
                        .chars()
                        .next()
                        .map(|c| c.len_utf8())
                        .unwrap_or(0);
                }
            }
            Key::Home | Key::CtrlA => self.cursor = 0,
            Key::End | Key::CtrlE => self.cursor = self.input.len(),
            Key::Up => {
                if self.hist_idx > 0 {
                    self.hist_idx -= 1;
                    self.input = self.hist[self.hist_idx].clone();
                    self.cursor = self.input.len();
                }
            }
            Key::Down => {
                if self.hist_idx < self.hist.len() {
                    self.hist_idx += 1;
                    self.input = if self.hist_idx == self.hist.len() {
                        String::new()
                    } else {
                        self.hist[self.hist_idx].clone()
                    };
                    self.cursor = self.input.len();
                }
            }
            Key::PageUp => {
                self.scroll = self.scroll.saturating_add(self.view_h.max(1));
            }
            Key::PageDown => {
                self.scroll = self.scroll.saturating_sub(self.view_h.max(1));
            }
            Key::Esc => {
                self.input.clear();
                self.cursor = 0;
            }
            Key::CtrlU => {
                self.input.clear();
                self.cursor = 0;
            }
            Key::CtrlW => {
                let upto = self.input[..self.cursor].trim_end().len();
                let word_start = self.input[..upto]
                    .char_indices()
                    .rev()
                    .find(|(_, c)| c.is_whitespace())
                    .map(|(i, c)| i + c.len_utf8())
                    .unwrap_or(0);
                self.input.replace_range(word_start..self.cursor, "");
                self.cursor = word_start;
            }
            Key::Char(c) => {
                self.input.insert(self.cursor, c);
                self.cursor += c.len_utf8();
            }
            Key::Ignore => {}
        }
    }

    fn del_at_cursor(&mut self) {
        if self.cursor < self.input.len() {
            let next = self.input[self.cursor..]
                .chars()
                .next()
                .map(|c| c.len_utf8())
                .unwrap_or(0);
            self.input.replace_range(self.cursor..self.cursor + next, "");
        }
    }

    fn run_slash(&mut self, line: &str, tx: &Sender<Ev>) {
        let mut it = line.split_whitespace();
        let cmd = it.next().unwrap_or("").to_ascii_lowercase();
        let args: Vec<&str> = it.collect();
        match cmd.as_str() {
            "/help" | "/h" | "/?" => self.sys(HELP),
            "/quit" | "/exit" | "/q" => self.running = false,
            "/clear" => self.transcript.clear(),
            "/model" => {
                if args.is_empty() {
                    self.sys(&format!("当前模型: {}，模式: {}", self.model, self.mode));
                } else {
                    self.model = args[0].to_string();
                    self.sys(&format!(
                        "模型已切到 {}（模式不变，仍是 {}）",
                        self.model, self.mode
                    ));
                }
            }
            "/agent" => self.set_mode("agent"),
            "/direct" => self.set_mode("direct"),
            "/side" => self.set_mode("side"),
            "/battle" => {
                self.set_mode("battle");
                if !args.is_empty() {
                    let q = args.join(" ");
                    self.send_text(q, tx);
                }
            }
            "/mode" => match args.first() {
                Some(&m) if ["agent", "direct", "battle", "side"].contains(&m) => self.set_mode(m),
                _ => self.sys("用法: /mode <agent|direct|battle|side>"),
            },
            "/panemodels" => {
                if args.is_empty() {
                    let cur = if self.pane_models.is_empty() {
                        "默认".to_string()
                    } else {
                        self.pane_models.join(", ")
                    };
                    self.sys(&format!("对战栏模型: {}（用法: /panemodels <a,b>）", cur));
                } else {
                    self.pane_models = args[0]
                        .split(',')
                        .map(|x| x.trim().to_string())
                        .filter(|x| !x.is_empty())
                        .collect();
                    self.sys(&format!("对战栏模型已设为: {}", self.pane_models.join(", ")));
                }
            }
            "/session" => {
                if args.is_empty() {
                    self.sys(&format!("当前会话: {}", self.session));
                } else {
                    self.session = args[0].to_string();
                    self.transcript.clear();
                    self.sys(&format!("已切到会话 {}（对话记录已清空）", self.session));
                }
            }
            "/new" => {
                let s = self.session.clone();
                let b = self.bridge.clone();
                self.transcript.clear();
                Self::spawn_job(tx, move || match b.reset(&s) {
                    Ok(_) => (format!("已清空会话 {} 的服务端历史", s), false),
                    Err(e) => (e, true),
                });
            }
            "/models" => {
                let b = self.bridge.clone();
                Self::spawn_job(tx, move || {
                    let mut out = String::new();
                    match b.models_v1() {
                        Ok(ms) => out.push_str(&format!("聊天模型: {}\n", ms.join(", "))),
                        Err(e) => return (e, true),
                    }
                    match b.models_routing() {
                        Ok(ms) if !ms.is_empty() => out.push_str(&format!("路由模型: {}", ms.join(", "))),
                        _ => {}
                    }
                    (out, false)
                });
            }
            "/health" => {
                let b = self.bridge.clone();
                Self::spawn_job(tx, move || match b.health() {
                    Ok(j) => (fmt_health(&j), false),
                    Err(e) => (e, true),
                });
            }
            "/repos" => {
                let b = self.bridge.clone();
                let s = self.session.clone();
                Self::spawn_job(tx, move || match b.repos(&s, false) {
                    Ok(j) => {
                        let cur = j.get("repo").and_then(|v| v.as_str()).unwrap_or("?");
                        let mut out = format!("当前仓库: {}", cur);
                        if let Some(arr) = j.get("repos").and_then(|v| v.as_arr()) {
                            for r in arr {
                                out.push_str(&format!("\n  {}", r.as_str().unwrap_or("?")));
                            }
                        }
                        (out, false)
                    }
                    Err(e) => (e, true),
                });
            }
            "/repo" => {
                if args.is_empty() {
                    self.sys("用法: /repo <owner/name>");
                } else {
                    let name = args.join(" ");
                    let b = self.bridge.clone();
                    let s = self.session.clone();
                    Self::spawn_job(tx, move || match b.switch_repo(&s, &name) {
                        Ok(j) => (
                            format!(
                                "已切到 {}，新对话已开",
                                j.get("repo").and_then(|v| v.as_str()).unwrap_or(&name)
                            ),
                            false,
                        ),
                        Err(e) => (e, true),
                    });
                }
            }
            "/reasoning" => {
                self.reasoning = match args.first() {
                    Some(&"on") => true,
                    Some(&"off") => false,
                    _ => !self.reasoning,
                };
                self.sys(&format!("思考过程显示: {}", if self.reasoning { "on" } else { "off" }));
            }
            "/upload" => {
                if args.is_empty() {
                    self.sys("用法: /upload <文件路径...>");
                } else {
                    let files: Vec<String> = args.iter().map(|s| s.to_string()).collect();
                    let b = self.bridge.clone();
                    let s = self.session.clone();
                    Self::spawn_job(tx, move || match b.upload(&s, &files) {
                        Ok(j) => {
                            let att = j
                                .get("attached")
                                .and_then(|v| v.as_arr())
                                .map(|a| {
                                    a.iter()
                                        .filter_map(|x| x.as_str().map(String::from))
                                        .collect::<Vec<_>>()
                                        .join(", ")
                                })
                                .unwrap_or_default();
                            (format!("已挂载到会话 {}: {}", s, att), false)
                        }
                        Err(e) => (e, true),
                    });
                }
            }
            "/vote" => {
                if args.is_empty() {
                    self.sys("用法: /vote <a|b|tie|both>");
                } else {
                    let choice = args[0].to_string();
                    let b = self.bridge.clone();
                    let s = self.session.clone();
                    Self::spawn_job(tx, move || match b.vote(&s, &choice) {
                        Ok(j) => (format!("已投票: {}", j.get("choice").and_then(|v| v.as_str()).unwrap_or(&choice)), false),
                        Err(e) => (e, true),
                    });
                }
            }
            _ => self.sys("未知命令，/help 查看帮助"),
        }
    }
}

// ---------- 渲染 ----------

fn char_w(c: char) -> usize {
    let u = c as u32;
    if (0x1100..=0x115F).contains(&u)
        || ((0x2E80..=0xA4CF).contains(&u) && u != 0x303F)
        || (0xAC00..=0xD7A3).contains(&u)
        || (0xF900..=0xFAFF).contains(&u)
        || (0xFE30..=0xFE4F).contains(&u)
        || (0xFF00..=0xFF60).contains(&u)
        || (0xFFE0..=0xFFE6).contains(&u)
        || (0x20000..=0x3FFFD).contains(&u)
    {
        2
    } else {
        1
    }
}

fn width_of(s: &str) -> usize {
    s.chars().map(char_w).sum()
}

fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut lines = Vec::new();
    let mut cur = String::new();
    let mut cur_w = 0;
    for c in text.chars() {
        if c == '\n' {
            lines.push(std::mem::take(&mut cur));
            cur_w = 0;
            continue;
        }
        let w = char_w(c);
        if cur_w + w > width && cur_w > 0 {
            lines.push(std::mem::take(&mut cur));
            cur_w = 0;
        }
        cur.push(c);
        cur_w += w;
    }
    lines.push(cur);
    lines
}

fn style_code(role: Role) -> &'static str {
    match role {
        Role::User => "\x1b[1;36m",
        Role::Assistant => "\x1b[1;32m",
        Role::Reasoning => "\x1b[2;90m",
        Role::System => "\x1b[34m",
        Role::Error => "\x1b[1;31m",
    }
}

fn render(app: &mut App, out: &mut impl Write) -> io::Result<()> {
    let (rows, cols) = Term::size();
    let width = cols.max(20);
    let view_h = rows.saturating_sub(2).max(1);
    app.view_h = view_h;

    // 把 transcript 展开成带角色的显示行
    let mut lines: Vec<(Role, String)> = Vec::new();
    for m in &app.transcript {
        let prefix = match m.role {
            Role::User => "你 › ",
            Role::Assistant => "arena ❯ ",
            Role::Reasoning => "· ",
            Role::System => "ℹ ",
            Role::Error => "✗ ",
        };
        let pw = width_of(prefix);
        for w in wrap(&m.text, width.saturating_sub(pw).max(1)) {
            lines.push((m.role, format!("{}{}", prefix, w)));
        }
    }

    let total = lines.len();
    let max_scroll = total.saturating_sub(view_h);
    let scroll = app.scroll.min(max_scroll);
    let start = total.saturating_sub(view_h + scroll);
    let end = (start + view_h).min(total);

    let mut s = String::new();
    s.push_str("\x1b[H");

    // 状态栏
    let ms = app.stream_start.map(|t| t.elapsed().as_millis()).unwrap_or(0);
    let elapsed = (ms / 1000) as u64;
    let spin = if app.streaming {
        format!(
            "{} {}s",
            SPINNER[(ms as usize / 100) % SPINNER.len()],
            elapsed
        )
    } else {
        String::new()
    };
    let dot = match app.bridge_ok {
        Some(true) => "●",
        Some(false) => "○",
        None => "?",
    };
    let left = format!(
        " arena-lite │ {} │ {} │ {} │ {}:{} {}",
        app.mode, app.model, app.session, app.bridge.ep.host, app.bridge.ep.port, dot
    );
    let right = if app.streaming { format!(" {} ", spin) } else { String::new() };
    let lw = width_of(&left);
    let rw = width_of(&right);
    s.push_str("\x1b[7m");
    s.push_str(&left);
    if lw + rw < width {
        for _ in 0..(width - lw - rw) {
            s.push(' ');
        }
    }
    s.push_str(&right);
    s.push_str("\x1b[0m\x1b[K\n");

    // 对话区
    for i in start..end {
        let (role, text) = &lines[i];
        s.push_str(style_code(*role));
        s.push_str(text);
        s.push_str("\x1b[0m\x1b[K\n");
    }
    for _ in end..(start + view_h) {
        s.push_str("\x1b[K\n");
    }

    // 输入行（过长时横向滚动，保证光标可见）
    let prompt = "› ";
    let avail = width.saturating_sub(width_of(prompt) + 1).max(1);
    let input = app.input.clone();
    let cursor = app.cursor.min(input.len());
    let cur_w = width_of(&input[..cursor]);
    let mut off = 0usize; // 显示窗口起点（显示宽度）
    if cur_w >= avail {
        off = cur_w + 1 - avail;
    }
    // 按显示宽度裁出可见段
    let mut vis = String::new();
    let mut w = 0usize;
    for c in input.chars() {
        let cw = char_w(c);
        if w + cw > off && w < off + avail {
            vis.push(c);
        }
        w += cw;
    }
    s.push_str("\x1b[0m");
    s.push_str(prompt);
    s.push_str(&vis);
    s.push_str("\x1b[K");
    let cur_col = width_of(prompt) + (cur_w - off) + 1;
    s.push_str(&format!("\x1b[{};{}H", rows, cur_col.max(1)));

    out.write_all(s.as_bytes())?;
    out.flush()
}

// ---------- 入口 ----------

pub fn run(cfg: &Config) -> i32 {
    if !Term::is_tty() {
        return repl(cfg);
    }
    let term = match Term::new() {
        Ok(t) => t,
        Err(e) => {
            eprintln!("无法进入终端 raw 模式（{}），退化为行模式", e);
            return repl(cfg);
        }
    };
    let code = run_tui(cfg);
    drop(term);
    code
}

fn run_tui(cfg: &Config) -> i32 {
    let bridge = match Bridge::new(&cfg.base_url, cfg.token.clone(), cfg.timeout) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("✗ {}", e);
            return 2;
        }
    };
    let (tx, rx) = channel::<Ev>();
    let keys = spawn_key_reader();
    let tx2 = tx.clone();
    thread::spawn(move || {
        while let Ok(k) = keys.recv() {
            if tx2.send(Ev::Key(k)).is_err() {
                break;
            }
        }
    });

    let mut app = App::new(cfg, bridge);
    app.spawn_health(&tx);
    app.sys(&format!(
        "已连接 {}（模型 {}，会话 {}）。输入问题回车发送，/help 查看命令。",
        cfg.base_url, app.model, app.session
    ));

    let mut out = io::stdout();
    let mut dirty = true;
    let mut last_render = Instant::now();
    while app.running {
        match rx.recv_timeout(Duration::from_millis(80)) {
            Ok(ev) => {
                app.handle(ev, &tx);
                dirty = true;
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        let now = Instant::now();
        if dirty || (app.streaming && now.duration_since(last_render) > Duration::from_millis(150)) {
            let _ = render(&mut app, &mut out);
            last_render = now;
            dirty = false;
        }
    }
    0
}

/// 非 TTY 环境下的行模式 REPL。
fn repl(cfg: &Config) -> i32 {
    eprintln!("非 TTY 环境，进入行模式（Ctrl-D 或 /quit 退出）");
    let bridge = match Bridge::new(&cfg.base_url, cfg.token.clone(), cfg.timeout) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("✗ {}", e);
            return 2;
        }
    };
    let stdin = io::stdin();
    let mut transcript: Vec<(String, String)> = Vec::new();
    let mut mode = cfg.mode.clone();
    loop {
        eprint!("{}[{}] ❯ ", cfg.session, mode);
        let _ = io::stderr().flush();
        let mut line = String::new();
        match stdin.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {}
            Err(_) => break,
        }
        let q = line.trim();
        if q.is_empty() {
            continue;
        }
        match q {
            "/quit" | "/exit" => break,
            "/agent" => {
                mode = "agent".to_string();
                eprintln!("→ agent 模式");
                continue;
            }
            "/direct" => {
                mode = "direct".to_string();
                eprintln!("→ direct 模式");
                continue;
            }
            "/battle" => {
                mode = "battle".to_string();
                eprintln!("→ battle 对战模式（之后每轮两栏并排回答，可 /vote 投票）");
                continue;
            }
            "/side" => {
                mode = "side".to_string();
                eprintln!("→ side-by-side 模式");
                continue;
            }
            _ => {}
        }
        if let Some(m) = q.strip_prefix("/mode ") {
            let m = m.trim();
            if ["agent", "direct", "battle", "side"].contains(&m) {
                mode = m.to_string();
                eprintln!("→ {} 模式", mode);
                continue;
            }
        }
        transcript.push(("user".to_string(), q.to_string()));
        let req = ChatRequest {
            model: cfg.model.clone(),
            session: cfg.session.clone(),
            mode: mode.clone(),
            fresh: cfg.fresh,
            messages: transcript.clone(),
            models: cfg.models.clone(),
        };
        let (tx, rx) = channel::<StreamEvent>();
        let b = bridge.clone();
        let th = thread::spawn(move || {
            let r = b.chat_stream(&req, |e| {
                let _ = tx.send(e);
            });
            if let Err(e) = r {
                let _ = tx.send(StreamEvent::Error(e));
            }
        });
        let stdout = io::stdout();
        let mut out = stdout.lock();
        let mut ans = String::new();
        let mut failed = false;
        loop {
            match rx.recv() {
                Ok(StreamEvent::Text(t)) => {
                    let _ = out.write_all(t.as_bytes());
                    let _ = out.flush();
                    ans.push_str(&t);
                }
                Ok(StreamEvent::Reasoning(t)) => {
                    if cfg.reasoning {
                        let _ = write!(io::stderr(), "\x1b[2m{}\x1b[0m", t);
                        let _ = io::stderr().flush();
                    }
                }
                Ok(StreamEvent::Error(e)) => {
                    let _ = writeln!(io::stderr(), "\n✗ {}", e);
                    failed = true;
                    break;
                }
                Ok(StreamEvent::Done) => break,
                Err(_) => break,
            }
        }
        let _ = th.join();
        let _ = writeln!(out);
        if !failed {
            transcript.push(("assistant".to_string(), ans));
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrap_cjk() {
        let lines = wrap("你好世界abc", 6);
        assert_eq!(lines, vec!["你好世".to_string(), "界abc".to_string()]);
    }

    #[test]
    fn width_counts_wide_chars() {
        assert_eq!(width_of("ab中文"), 6);
        assert_eq!(width_of("🎉"), 2);
    }

    #[test]
    fn parse_arrow_and_enter() {
        let mut buf = b"\x1b[A\r".to_vec();
        let keys = parse_keys(&mut buf);
        assert_eq!(keys, vec![Key::Up, Key::Enter]);
        assert!(buf.is_empty());
    }
}
