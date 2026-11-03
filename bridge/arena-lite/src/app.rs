//! TUI 的状态机：输入编辑、斜杠命令、流式回合、滚动跟随。
//!
//! 这里不碰终端（不打印、不读键），只改状态；渲染在 `ui.rs`。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::client::{ChatOpts, Client, Health, History, Msg as ChatMsg, StreamEvent};
use crate::err::{truncate, BridgeError};
use crate::event::{Msg, SessionInfo};
use crate::keys::Key;
use crate::mode::Mode;

/// 一次发送给桥的对话最多带多少条历史（再多就是在浪费 prompt 长度）
const MAX_HISTORY_MSGS: usize = 12;
pub const NOTICE_TTL: Duration = Duration::from_secs(6);

#[derive(Clone, Debug)]
pub enum Block {
    User {
        text: String,
    },
    Assistant {
        text: String,
    },
    Reasoning {
        text: String,
        lines: usize,
    },
    Tool {
        name: String,
        input: String,
    },
    System {
        text: String,
    },
    Error {
        text: String,
    },
    Verify {
        text: String,
    },
}

pub enum HealthState {
    Unknown,
    Up(Health),
    Down(String),
}

pub struct StreamState {
    pub id: u64,
    pub started: Instant,
    pub verifying: bool,
    /// 有没有收到过正文/思考（用于状态栏文案）
    pub got_output: bool,
}

#[derive(Clone)]
pub struct Config {
    pub base_display: String,
    pub session: String,
    pub mode: Mode,
    pub models: Vec<String>,
    pub reasoning_open: bool,
}

pub struct App {
    pub client: Arc<Client>,
    pub tx: Sender<Msg>,
    pub cfg: Config,

    pub blocks: Vec<Block>,
    /// 真正发给桥的对话（user/assistant 交替）
    pub log: Vec<ChatMsg>,

    pub input: String,
    /// 光标位置（按字符计，不是字节）
    pub cursor: usize,
    pub hist: Vec<String>,
    pub hist_pos: Option<usize>,
    pub draft: String,

    /// 不跟随时，对话窗口的顶行索引
    pub scroll_top: usize,
    pub follow: bool,
    pub view_h: usize,

    pub show_reasoning: bool,
    pub show_tools: bool,

    pub health: HealthState,
    pub stream: Option<StreamState>,
    pub notice: Option<(String, Instant)>,

    pub gen_id: u64,
    pub abort: Arc<AtomicBool>,
    pub quit: bool,
    pub dirty: bool,

    pub sessions: Vec<SessionInfo>,
    pub pending_pick: Option<Vec<String>>,
    pub last_prompt: Option<String>,
}

impl App {
    pub fn new(client: Arc<Client>, tx: Sender<Msg>, cfg: Config) -> App {
        let show_reasoning = cfg.reasoning_open;
        App {
            client,
            tx,
            cfg,
            blocks: Vec::new(),
            log: Vec::new(),
            input: String::new(),
            cursor: 0,
            hist: Vec::new(),
            hist_pos: None,
            draft: String::new(),
            scroll_top: 0,
            follow: true,
            view_h: 10,
            show_reasoning,
            show_tools: false,
            health: HealthState::Unknown,
            stream: None,
            notice: None,
            gen_id: 0,
            abort: Arc::new(AtomicBool::new(false)),
            quit: false,
            dirty: true,
            sessions: Vec::new(),
            pending_pick: None,
            last_prompt: None,
        }
    }

    /* ------------------------------ 状态查询 ------------------------------ */

    pub fn busy(&self) -> bool {
        self.stream.is_some()
    }

    pub fn mode(&self) -> Mode {
        self.cfg.mode
    }

    pub fn model_label(&self) -> String {
        if self.cfg.mode.is_dual() && !self.cfg.models.is_empty() {
            let mut parts: Vec<String> = Vec::new();
            for m in self.cfg.models.iter().take(2) {
                parts.push(m.clone());
            }
            while parts.len() < 2 {
                parts.push("auto".to_string());
            }
            parts.join(" vs ")
        } else {
            self.cfg.mode.default_model().to_string()
        }
    }

    /// 提示是否已经过期（主循环用它决定要不要再画一帧）
    pub fn notice_expired(&self) -> bool {
        match &self.notice {
            Some((_, at)) => at.elapsed() > NOTICE_TTL,
            None => false,
        }
    }

    pub fn notice(&mut self, text: impl Into<String>) {
        self.notice = Some((text.into(), Instant::now()));
        self.dirty = true;
    }

    pub fn push_system(&mut self, text: impl Into<String>) {
        self.blocks.push(Block::System { text: text.into() });
        self.dirty = true;
        if self.follow {
            self.scroll_top = usize::MAX; // 渲染时会被夹回底部
        }
    }

    pub fn push_error(&mut self, text: impl Into<String>) {
        self.blocks.push(Block::Error { text: text.into() });
        self.dirty = true;
    }

    fn touch_reasoning(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        if let Some(Block::Reasoning { text: t, lines }) = self.blocks.last_mut() {
            t.push_str(text);
            *lines = t.split('\n').count();
            return;
        }
        let lines = text.split('\n').count();
        self.blocks.push(Block::Reasoning {
            text: text.to_string(),
            lines,
        });
    }

    fn touch_tool(&mut self, name: String, input: String) {
        self.blocks.push(Block::Tool { name, input });
    }

    fn append_text(&mut self, text: &str) {
        if let Some(Block::Assistant { text: t }) = self.blocks.last_mut() {
            t.push_str(text);
            return;
        }
        self.blocks.push(Block::Assistant {
            text: text.to_string(),
        });
    }

    /* ------------------------------ 输入编辑 ------------------------------ */

    pub fn input_lines(&self) -> usize {
        self.input.split('\n').count().max(1)
    }

    fn set_input(&mut self, s: String) {
        self.input = s;
        self.cursor = self.input.chars().count();
        self.hist_pos = None;
        self.dirty = true;
    }

    fn insert_char(&mut self, c: char) {
        let byte = self.byte_of_cursor();
        self.input.insert(byte, c);
        self.cursor += 1;
        self.hist_pos = None;
        self.dirty = true;
    }

    fn byte_of_cursor(&self) -> usize {
        self.input
            .char_indices()
            .nth(self.cursor)
            .map(|(i, _)| i)
            .unwrap_or(self.input.len())
    }

    fn delete_back(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let start = self
            .input
            .char_indices()
            .nth(self.cursor - 1)
            .map(|(i, _)| i)
            .unwrap_or(0);
        let end = self.byte_of_cursor();
        self.input.replace_range(start..end, "");
        self.cursor -= 1;
        self.hist_pos = None;
        self.dirty = true;
    }

    fn delete_forward(&mut self) {
        let start = self.byte_of_cursor();
        if start >= self.input.len() {
            return;
        }
        let end = self
            .input
            .char_indices()
            .nth(self.cursor + 1)
            .map(|(i, _)| i)
            .unwrap_or(self.input.len());
        self.input.replace_range(start..end, "");
        self.dirty = true;
    }

    fn move_left(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
            self.dirty = true;
        }
    }

    fn move_right(&mut self) {
        if self.cursor < self.input.chars().count() {
            self.cursor += 1;
            self.dirty = true;
        }
    }

    fn hist_prev(&mut self) {
        if self.hist.is_empty() {
            return;
        }
        let pos = match self.hist_pos {
            None => {
                self.draft = self.input.clone();
                self.hist.len() - 1
            }
            Some(0) => return,
            Some(p) => p - 1,
        };
        self.hist_pos = Some(pos);
        let entry = self.hist[pos].clone();
        self.input = entry;
        self.cursor = self.input.chars().count();
        self.dirty = true;
    }

    fn hist_next(&mut self) {
        let pos = match self.hist_pos {
            None => return,
            Some(p) => p,
        };
        if pos + 1 >= self.hist.len() {
            self.hist_pos = None;
            let draft = std::mem::take(&mut self.draft);
            self.input = draft;
            self.cursor = self.input.chars().count();
            self.dirty = true;
            return;
        }
        self.hist_pos = Some(pos + 1);
        let entry = self.hist[pos + 1].clone();
        self.input = entry;
        self.cursor = self.input.chars().count();
        self.dirty = true;
    }

    fn clear_input(&mut self) {
        if !self.input.is_empty() {
            self.input.clear();
            self.cursor = 0;
            self.hist_pos = None;
            self.dirty = true;
        }
    }

    /* ------------------------------ 滚动 ------------------------------ */

    pub fn scroll_page(&mut self, up: bool) {
        let step = self.view_h.saturating_sub(1).max(1);
        if up {
            let cur = if self.follow {
                // 从底部开始：先算当前顶行，再上移一页
                self.scroll_top.max(0)
            } else {
                self.scroll_top
            };
            self.scroll_top = cur.saturating_sub(step);
            self.follow = false;
        } else {
            self.scroll_top = self.scroll_top.saturating_add(step);
            // 是否已经到底会在渲染时确认
            self.follow = false;
        }
        self.dirty = true;
    }

    /// 渲染时把「顶行」校准：到底了就恢复跟随
    pub fn anchor(&mut self, total_lines: usize) {
        let h = self.view_h;
        if self.follow {
            self.scroll_top = total_lines.saturating_sub(h);
            return;
        }
        let max_top = total_lines.saturating_sub(h);
        if self.scroll_top >= max_top {
            self.scroll_top = max_top;
            self.follow = true;
        } else {
            self.scroll_top = self.scroll_top.min(max_top);
        }
    }

    /* ------------------------------ 发送 ------------------------------ */

    pub fn submit(&mut self) {
        if self.busy() {
            self.notice("正在生成，先 Ctrl+C 中断再发");
            return;
        }
        let raw = self.input.trim().to_string();
        if raw.is_empty() {
            return;
        }

        // 会话列表里选了编号
        if let Some(list) = self.pending_pick.clone() {
            if let Ok(n) = raw.parse::<usize>() {
                if n >= 1 && n <= list.len() {
                    self.pending_pick = None;
                    let name = list[n - 1].clone();
                    self.set_input(String::new());
                    self.load_session(&name);
                    return;
                }
            }
            self.pending_pick = None;
        }

        // 用户主动发东西 → 回到跟随状态（之前可能是上滚在看老内容）
        self.follow = true;

        if raw.starts_with('/') {
            self.set_input(String::new());
            self.run_command(&raw);
            return;
        }

        self.hist.push(raw.clone());
        if self.hist.len() > 200 {
            self.hist.remove(0);
        }
        self.hist_pos = None;
        self.set_input(String::new());
        self.push_prompt(raw);
    }

    /// 把一条用户消息送出去（也会被 /battle <问题> 复用）
    pub fn push_prompt(&mut self, prompt: String) {
        self.last_prompt = Some(prompt.clone());
        self.blocks.push(Block::User {
            text: prompt.clone(),
        });
        self.log.push(ChatMsg::user(prompt));
        if self.follow {
            self.scroll_top = usize::MAX;
        }

        self.gen_id += 1;
        let id = self.gen_id;
        self.abort = Arc::new(AtomicBool::new(false));
        self.stream = Some(StreamState {
            id,
            started: Instant::now(),
            verifying: false,
            got_output: false,
        });
        self.dirty = true;

        // 只带最近若干条历史，太长没意义
        let start = self.log.len().saturating_sub(MAX_HISTORY_MSGS);
        let msgs: Vec<ChatMsg> = self.log[start..].to_vec();
        let mut opts = ChatOpts::new(self.cfg.mode, self.cfg.session.clone());
        opts.models = self.cfg.models.clone();

        spawn_chat(
            Arc::clone(&self.client),
            self.tx.clone(),
            id,
            msgs,
            opts,
            Arc::clone(&self.abort),
        );
    }

    pub fn abort_current(&mut self) {
        if self.stream.is_some() {
            self.abort.store(true, Ordering::SeqCst);
            self.stream = None;
            self.push_system("已中断本次生成");
            // 把本轮用户消息从 log 里摘掉，避免下一轮重复喂给模型
            if let Some(ChatMsg {
                role,
                content: _,
            }) = self.log.last()
            {
                if role == "user" {
                    self.log.pop();
                }
            }
            self.dirty = true;
        }
    }

    /* ------------------------------ 流式事件 ------------------------------ */

    pub fn on_stream(&mut self, id: u64, ev: StreamEvent) {
        let current = match &self.stream {
            Some(s) => s.id == id,
            None => false,
        };
        if !current {
            return; // 旧回合的残余
        }
        if let Some(s) = &mut self.stream {
            s.got_output = true;
        }
        match ev {
            StreamEvent::Text(t) => self.append_text(&t),
            StreamEvent::Reasoning(t) => self.touch_reasoning(&t),
            StreamEvent::Tool { name, input } => self.touch_tool(name, input),
        }
        self.dirty = true;
    }

    pub fn on_stream_end(&mut self, id: u64, result: Result<(), BridgeError>) {
        let current = match &self.stream {
            Some(s) => s.id == id,
            None => false,
        };
        if !current {
            return;
        }
        self.stream = None;
        self.dirty = true;
        match result {
            Ok(()) => {
                // 收尾：把助手正文入档
                let text = match self.blocks.last() {
                    Some(Block::Assistant { text }) => text.clone(),
                    _ => String::new(),
                };
                // 没有任何正文（例如双栏只回了思考）就不入档，别在 log 里留空档
                if !text.trim().is_empty() {
                    self.log.push(ChatMsg::assistant(text));
                }
            }
            Err(e) => {
                if !matches!(e, BridgeError::Aborted) {
                    self.blocks.push(Block::Error {
                        text: e.human(),
                    });
                }
                // 本轮失败：把 user 消息从 log 摘掉，下次重发不会重复
                if let Some(ChatMsg { role, .. }) = self.log.last() {
                    if role == "user" {
                        self.log.pop();
                    }
                }
            }
        }
        if self.follow {
            self.scroll_top = usize::MAX;
        }
    }

    pub fn on_need_verify(&mut self, id: u64) {
        let current = match &self.stream {
            Some(s) => s.id == id,
            None => false,
        };
        if !current {
            return;
        }
        if let Some(s) = &mut self.stream {
            s.verifying = true;
        }
        self.blocks.push(Block::Verify {
            text: "需要人工验证，已切到浏览器\n（过完验证会自动重试这一次请求，最多一次）".to_string(),
        });
        self.dirty = true;
    }

    /* ------------------------------ 后台结果 ------------------------------ */

    pub fn on_health(&mut self, res: Result<Health, BridgeError>) {
        self.health = match res {
            Ok(h) => HealthState::Up(h),
            Err(e) => HealthState::Down(e.short().to_string()),
        };
        self.dirty = true;
    }

    pub fn on_session_list(&mut self, list: Vec<SessionInfo>) {
        self.sessions = list.clone();
        self.pending_pick = Some(list.iter().map(|s| s.name.clone()).collect());
        if list.is_empty() {
            self.push_system("桥上没有打开的会话（先随便问一句，会话才会建起来）");
            self.pending_pick = None;
            return;
        }
        self.push_system("可用会话（输入编号或 /resume <名字> 载入）：");
        for (i, s) in list.iter().enumerate() {
            let when = s.at.map(format_relative).unwrap_or_else(|| "时间未知".into());
            let last = if s.last.trim().is_empty() {
                "（空会话）".to_string()
            } else {
                truncate(s.last.trim(), 40)
            };
            let extra = match &s.error {
                Some(e) => format!("  ⚠ {}", truncate(e, 40)),
                None => String::new(),
            };
            self.push_system(format!(
                "  {}. {}  ·  {} 条 · {}  ·  {}{}",
                i + 1,
                s.name,
                s.turns,
                when,
                last,
                extra
            ));
        }
    }

    pub fn on_history(&mut self, res: Result<History, BridgeError>) {
        match res {
            Ok(h) => {
                if h.messages.is_empty() {
                    self.push_system(format!("会话 {} 里没读到消息", h.session));
                    return;
                }
                self.blocks.clear();
                self.log.clear();
                for m in &h.messages {
                    let role = m.role.to_ascii_lowercase();
                    if role.contains("user") {
                        self.blocks.push(Block::User {
                            text: m.text.clone(),
                        });
                        self.log.push(ChatMsg::user(m.text.clone()));
                    } else {
                        self.blocks.push(Block::Assistant {
                            text: m.text.clone(),
                        });
                        self.log.push(ChatMsg::assistant(m.text.clone()));
                    }
                }
                self.follow = true;
                self.scroll_top = usize::MAX;
                let counted = if h.turns > 0 {
                    h.turns as usize
                } else {
                    h.messages.len()
                };
                self.push_system(format!(
                    "已载入会话 {}（{} 条，实际装载 {} 条），之后的提问会接着它",
                    h.session,
                    counted,
                    h.messages.len()
                ));
                if h.via == "alternate" {
                    self.push_system(
                        "注意：页面结构里没有角色标记，这份历史的「谁说的」是按顺序猜的，可能不准",
                    );
                }
                self.dirty = true;
            }
            Err(e) => {
                self.push_error(format!("读历史失败：{}", e.human()));
            }
        }
    }

    /* ------------------------------ 斜杠命令 ------------------------------ */

    pub fn run_command(&mut self, raw: &str) {
        let (cmd, rest) = match raw.split_once(char::is_whitespace) {
            Some((c, r)) => (c.to_string(), r.trim().to_string()),
            None => (raw.to_string(), String::new()),
        };
        let cmd_lower = cmd.to_ascii_lowercase();
        match cmd_lower.as_str() {
            "/help" | "/?" | "/h" => self.show_help(),
            "/agent" => self.set_mode(Mode::Agent),
            "/direct" => self.set_mode(Mode::Direct),
            "/side" => self.set_mode(Mode::Side),
            "/battle" => {
                if rest.is_empty() {
                    self.set_mode(Mode::Battle);
                } else if self.busy() {
                    self.notice("正在生成，先等这一轮结束");
                } else {
                    self.cfg.mode = Mode::Battle;
                    self.push_prompt(rest);
                }
            }
            "/mode" => match Mode::parse(&rest) {
                Some(m) => self.set_mode(m),
                None => {
                    let names: Vec<&str> = Mode::all().iter().map(|m| m.as_str()).collect();
                    self.notice(format!("用法：/mode {}", names.join("|")));
                }
            },
            "/models" => {
                if rest.is_empty() {
                    self.notice(format!("当前模型：{}（用 /models a,b 指定双栏两栏）", self.model_label()));
                } else {
                    self.cfg.models = rest
                        .split(',')
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .take(2)
                        .collect();
                    self.notice(format!("模型已设为 {}", self.model_label()));
                }
            }
            "/session" => {
                if rest.is_empty() {
                    self.notice(format!("当前会话：{}", self.cfg.session));
                } else {
                    self.cfg.session = rest;
                    self.notice(format!("会话已切到 {}（历史仍以本地对话为准）", self.cfg.session));
                }
            }
            "/resume" => {
                if rest.is_empty() {
                    self.list_sessions();
                } else {
                    self.load_session(&rest);
                }
            }
            "/sessions" => self.list_sessions(),
            "/new" => {
                let session = self.cfg.session.clone();
                let client = Arc::clone(&self.client);
                let tx = self.tx.clone();
                self.blocks.clear();
                self.log.clear();
                self.follow = true;
                self.push_system(format!("已清空会话 {}", session));
                std::thread::spawn(move || match client.new_session(&session) {
                    Ok(()) => {
                        let _ = tx.send(Msg::Notice("服务端会话已清空".into()));
                    }
                    Err(e) => {
                        let _ = tx.send(Msg::Notice(format!("清空失败：{}", e.short())));
                    }
                });
            }
            "/health" => self.fetch_health(),
            "/repos" => {
                let client = Arc::clone(&self.client);
                let tx = self.tx.clone();
                std::thread::spawn(move || match client.repos() {
                    Ok((cur, list)) => {
                        let _ = tx.send(Msg::Notice(format!(
                            "当前仓库 {}；可选 {}",
                            if cur.is_empty() { "（未选）" } else { &cur },
                            if list.is_empty() {
                                "（列表为空）".to_string()
                            } else {
                                list.join(" / ")
                            }
                        )));
                    }
                    Err(e) => {
                        let _ = tx.send(Msg::Notice(format!("读仓库列表失败：{}", e.short())));
                    }
                });
            }
            "/repo" => {
                if rest.is_empty() {
                    self.notice("用法：/repo <owner/name>（等于开新对话）");
                    return;
                }
                let client = Arc::clone(&self.client);
                let tx = self.tx.clone();
                let session = self.cfg.session.clone();
                self.blocks.clear();
                self.log.clear();
                self.push_system(format!("正在切到仓库 {} …（这等于开新对话）", rest));
                std::thread::spawn(move || match client.switch_repo(&rest, &session) {
                    Ok(_) => {
                        let _ = tx.send(Msg::Notice(format!("已切到 {}", rest)));
                    }
                    Err(e) => {
                        let _ = tx.send(Msg::Notice(format!("切仓库失败：{}", e.short())));
                    }
                });
            }
            "/vote" => {
                let choice = rest.to_ascii_lowercase();
                if !matches!(choice.as_str(), "a" | "b" | "tie" | "both") {
                    self.notice("用法：/vote a|b|tie|both");
                    return;
                }
                let client = Arc::clone(&self.client);
                let tx = self.tx.clone();
                let session = self.cfg.session.clone();
                std::thread::spawn(move || match client.vote(&choice, &session) {
                    Ok(msg) => {
                        let _ = tx.send(Msg::Notice(msg));
                    }
                    Err(e) => {
                        let _ = tx.send(Msg::Notice(format!("投票失败：{}", e.human())));
                    }
                });
            }
            "/upload" => {
                if rest.is_empty() {
                    self.notice("用法：/upload <本地文件路径>");
                    return;
                }
                let client = Arc::clone(&self.client);
                let tx = self.tx.clone();
                let session = self.cfg.session.clone();
                let files: Vec<String> = rest
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
                self.notice(format!("正在上传 {} 个文件…", files.len()));
                std::thread::spawn(move || match client.upload(&files, &session) {
                    Ok(msg) => {
                        let _ = tx.send(Msg::Notice(msg));
                    }
                    Err(e) => {
                        let _ = tx.send(Msg::Notice(format!("上传失败：{}", e.human())));
                    }
                });
            }
            "/reasoning" => {
                self.show_reasoning = !self.show_reasoning;
                self.notice(if self.show_reasoning {
                    "思考链已展开"
                } else {
                    "思考链已折叠"
                });
            }
            "/tools" => {
                self.show_tools = !self.show_tools;
                self.notice(if self.show_tools {
                    "工具块已展开"
                } else {
                    "工具块已折叠"
                });
            }
            "/clear" => {
                self.blocks.clear();
                self.notice("屏幕已清空（服务端上下文保留）");
            }
            "/retry" => {
                if self.busy() {
                    self.notice("还在生成中");
                    return;
                }
                match self.last_prompt.clone() {
                    Some(p) => {
                        if let Some(ChatMsg { role, .. }) = self.log.last() {
                            if role == "user" {
                                self.log.pop();
                            }
                        }
                        self.push_prompt(p);
                    }
                    None => self.notice("没有可以重试的提问"),
                }
            }
            "/quit" | "/exit" | "/q" => {
                self.quit = true;
            }
            other => {
                self.notice(format!("没有这个命令：{}（/help 看列表）", other));
            }
        }
    }

    pub fn set_mode(&mut self, m: Mode) {
        self.cfg.mode = m;
        self.notice(format!("模式 → {}：{}", m.label(), m.describe()));
        if self.follow {
            self.scroll_top = usize::MAX;
        }
    }

    fn show_help(&mut self) {
        self.push_system("斜杠命令：");
        self.push_system("  /agent /direct /side   切换常驻模式（切完一直沿用）");
        self.push_system("  /battle [问题]         对战；带问题就直接用 battle 发出去");
        self.push_system("  /mode <agent|direct|side|battle>");
        self.push_system("  /models [a,b]          查看/设置双栏两栏模型");
        self.push_system("  /session <名字>        切会话名");
        self.push_system("  /resume [会话]         列出会话并载入网页端历史");
        self.push_system("  /new                   清空服务端会话");
        self.push_system("  /health                体检桥");
        self.push_system("  /repos  /repo <名>     查看/切换仓库");
        self.push_system("  /vote a|b|tie|both     对战投票");
        self.push_system("  /upload <路径>         上传文件到当前会话");
        self.push_system("  /reasoning /tools      展开或折叠思考链 / 工具块");
        self.push_system("  /clear                 清屏（不动服务端上下文）");
        self.push_system("  /retry                 重发上一问");
        self.push_system("  /quit                  退出");
        self.push_system("键位：Enter 发送 · Shift+Enter/Alt+Enter 换行 · Esc 清空 · ↑↓ 历史");
        self.push_system("      PgUp/PgDn 滚动 · Ctrl+R 思考 · Ctrl+T 工具 · Ctrl+L 清屏");
    }

    pub fn fetch_health(&mut self) {
        let client = Arc::clone(&self.client);
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let res = client.health();
            let _ = tx.send(Msg::Health(res));
        });
    }

    fn list_sessions(&mut self) {
        self.push_system("正在读会话列表…");
        let client = Arc::clone(&self.client);
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let health = match client.health() {
                Ok(h) => h,
                Err(e) => {
                    let _ = tx.send(Msg::Notice(format!("读会话列表失败：{}", e.short())));
                    return;
                }
            };
            let mut infos: Vec<SessionInfo> = Vec::new();
            for name in health.sessions.iter().take(20) {
                match client.history(name) {
                    Ok(h) => {
                        let last = h
                            .messages
                            .last()
                            .map(|m| m.text.clone())
                            .unwrap_or_default();
                        infos.push(SessionInfo {
                            name: name.clone(),
                            at: h.at,
                            last,
                            turns: h.messages.len(),
                            error: None,
                        });
                    }
                    Err(e) => infos.push(SessionInfo {
                        name: name.clone(),
                        at: None,
                        last: String::new(),
                        turns: 0,
                        error: Some(e.short().to_string()),
                    }),
                }
            }
            let _ = tx.send(Msg::SessionList(infos));
        });
    }

    fn load_session(&mut self, name: &str) {
        self.cfg.session = name.to_string();
        self.push_system(format!("正在读会话 {} 的网页端历史…", name));
        let client = Arc::clone(&self.client);
        let tx = self.tx.clone();
        let name = name.to_string();
        std::thread::spawn(move || {
            let res = client.history(&name);
            let _ = tx.send(Msg::History(res));
        });
    }

    /* ------------------------------ Tab 补全 ------------------------------ */

    pub fn slash_commands() -> &'static [&'static str] {
        &[
            "/agent", "/battle", "/clear", "/direct", "/exit", "/health", "/help", "/mode",
            "/models", "/new", "/quit", "/reasoning", "/repo", "/repos", "/resume", "/retry",
            "/session", "/sessions", "/side", "/tools", "/upload", "/vote",
        ]
    }

    pub fn complete(&mut self) {
        if !self.input.starts_with('/') || self.input.contains(' ') {
            return;
        }
        let matches: Vec<&'static str> = Self::slash_commands()
            .iter()
            .copied()
            .filter(|c| c.starts_with(self.input.as_str()))
            .collect();
        match matches.len() {
            0 => self.notice("没有匹配的命令"),
            1 => {
                let done = format!("{} ", matches[0]);
                self.set_input(done);
            }
            _ => {
                // 补到公共前缀
                let common = common_prefix(&matches);
                if common.len() > self.input.len() {
                    self.set_input(common);
                } else {
                    self.notice(matches.join("  "));
                }
            }
        }
    }

    /* ------------------------------ 主循环入口 ------------------------------ */

    pub fn handle_key(&mut self, k: Key) {
        match k {
            Key::Ctrl('c') => {
                if self.busy() {
                    self.abort_current();
                } else {
                    self.quit = true;
                }
            }
            Key::Ctrl('r') => {
                self.show_reasoning = !self.show_reasoning;
                self.dirty = true;
            }
            Key::Ctrl('t') => {
                self.show_tools = !self.show_tools;
                self.dirty = true;
            }
            Key::Ctrl('l') => {
                self.blocks.clear();
                self.notice("屏幕已清空（服务端上下文保留）");
            }
            Key::Ctrl('n') => self.list_sessions(),
            Key::Ctrl('d') => self.quit = true,
            Key::PageUp => self.scroll_page(true),
            Key::PageDown => self.scroll_page(false),
            Key::Enter => self.submit(),
            Key::Newline | Key::Ctrl('j') => self.insert_char('\n'),
            Key::Backspace => self.delete_back(),
            Key::Delete => self.delete_forward(),
            Key::Left => self.move_left(),
            Key::Right => self.move_right(),
            Key::Home => {
                self.cursor = 0;
                self.dirty = true;
            }
            Key::End => {
                self.cursor = self.input.chars().count();
                self.dirty = true;
            }
            Key::Up => {
                if self.input_lines() <= 1 {
                    self.hist_prev();
                }
            }
            Key::Down => {
                if self.input_lines() <= 1 {
                    self.hist_next();
                }
            }
            Key::Tab => self.complete(),
            Key::Esc => self.clear_input(),
            Key::Char(c) => self.insert_char(c),
            Key::BackTab | Key::Ignored | Key::Ctrl(_) => {}
        }
    }
}

fn common_prefix(items: &[&'static str]) -> String {
    let mut out = match items.first() {
        Some(f) => f.to_string(),
        None => return String::new(),
    };
    for it in items.iter().skip(1) {
        while !it.starts_with(&out) {
            out.pop();
            if out.is_empty() {
                return out;
            }
        }
    }
    out
}

/// 相对时间（不依赖时区数据）
pub fn format_relative(ms: i64) -> String {
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let diff = now_ms - ms;
    if ms <= 0 {
        return "时间未知".to_string();
    }
    if diff < 0 {
        return "刚刚".to_string();
    }
    let secs = diff / 1000;
    if secs < 60 {
        "刚刚".to_string()
    } else if secs < 3600 {
        format!("{} 分钟前", secs / 60)
    } else if secs < 86400 {
        format!("{} 小时前", secs / 3600)
    } else {
        format!("{} 天前", secs / 86400)
    }
}

/// 一条提问的完整生命周期（含验证后自动重试一次）
fn spawn_chat(
    client: Arc<Client>,
    tx: Sender<Msg>,
    id: u64,
    msgs: Vec<ChatMsg>,
    opts: ChatOpts,
    abort: Arc<AtomicBool>,
) {
    let tx_fallback = tx.clone();
    let handle = std::thread::Builder::new()
        .name("arena-stream".to_string())
        .spawn(move || {
            let mut attempt = 0u32;
            loop {
                attempt += 1;
                let aborted = Arc::clone(&abort);
                let txc = tx.clone();
                let mut sink = move |ev: StreamEvent| -> bool {
                    if aborted.load(Ordering::SeqCst) {
                        return false;
                    }
                    txc.send(Msg::Stream { id, ev }).is_ok()
                };
                let result = client.chat_stream(&msgs, &opts, &mut sink);
                match result {
                    Ok(()) => {
                        let _ = tx.send(Msg::StreamEnd { id, result: Ok(()) });
                        return;
                    }
                    Err(BridgeError::Aborted) => {
                        let _ = tx.send(Msg::StreamEnd {
                            id,
                            result: Err(BridgeError::Aborted),
                        });
                        return;
                    }
                    Err(e) if e.wants_verify() => {
                        let _ = tx.send(Msg::NeedVerify { id });
                        // 顺手把 Chrome 窗口也拉到前台（桥侧已经切了标签页）
                        let _ = crate::foreground::bring_browser_forward();
                        if attempt >= 2 {
                            let _ = tx.send(Msg::StreamEnd {
                                id,
                                result: Err(BridgeError::NeedVerify),
                            });
                            return;
                        }
                        // 等人在浏览器里点完验证，最多等 45 秒
                        let mut waited = 0u32;
                        while waited < 45 {
                            std::thread::sleep(Duration::from_millis(500));
                            waited += 1;
                            if abort.load(Ordering::SeqCst) {
                                let _ = tx.send(Msg::StreamEnd {
                                    id,
                                    result: Err(BridgeError::Aborted),
                                });
                                return;
                            }
                            if waited >= 12 {
                                // 6 秒后开始试：健康检查能过就当验证结束
                                match client.health() {
                                    Ok(h) if h.chrome => break,
                                    _ => {}
                                }
                            }
                        }
                        continue;
                    }
                    Err(e) => {
                        let _ = tx.send(Msg::StreamEnd { id, result: Err(e) });
                        return;
                    }
                }
            }
        });
    if handle.is_err() {
        // 线程起不来也要给人话，而不是静默失败
        let _ = tx_fallback.send(Msg::StreamEnd {
            id,
            result: Err(BridgeError::Connect(
                "内部错误：起不了网络线程".to_string(),
            )),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dummy_app(mode: Mode) -> App {
        let base = crate::httpc::parse_base("127.0.0.1:17892").expect("base");
        let client = Arc::new(Client::new(base, None));
        let (tx, _rx) = std::sync::mpsc::channel();
        let cfg = Config {
            base_display: "127.0.0.1:17892".into(),
            session: "main".into(),
            mode,
            models: Vec::new(),
            reasoning_open: false,
        };
        App::new(client, tx, cfg)
    }

    #[test]
    fn typing_and_editing() {
        let mut app = dummy_app(Mode::Agent);
        for c in "你好abc".chars() {
            app.handle_key(Key::Char(c));
        }
        assert_eq!(app.input, "你好abc");
        assert_eq!(app.cursor, 5);
        app.handle_key(Key::Backspace);
        assert_eq!(app.input, "你好ab");
        app.handle_key(Key::Home);
        app.handle_key(Key::Delete);
        assert_eq!(app.input, "好ab");
        app.handle_key(Key::End);
        app.handle_key(Key::Left);
        app.handle_key(Key::Char('X'));
        assert_eq!(app.input, "好aXb");
        assert_eq!(app.cursor, 3);
    }

    #[test]
    fn esc_clears_input() {
        let mut app = dummy_app(Mode::Agent);
        app.handle_key(Key::Char('a'));
        app.handle_key(Key::Esc);
        assert_eq!(app.input, "");
    }

    #[test]
    fn history_navigation() {
        let mut app = dummy_app(Mode::Agent);
        app.hist = vec!["一".into(), "二".into()];
        app.handle_key(Key::Up);
        assert_eq!(app.input, "二");
        app.handle_key(Key::Up);
        assert_eq!(app.input, "一");
        app.handle_key(Key::Up);
        assert_eq!(app.input, "一");
        app.handle_key(Key::Down);
        assert_eq!(app.input, "二");
        app.handle_key(Key::Down);
        assert_eq!(app.input, "");
    }

    #[test]
    fn multiline_input_uses_newline_key() {
        let mut app = dummy_app(Mode::Agent);
        app.handle_key(Key::Char('a'));
        app.handle_key(Key::Newline);
        app.handle_key(Key::Char('b'));
        assert_eq!(app.input, "a\nb");
        assert_eq!(app.input_lines(), 2);
    }

    #[test]
    fn mode_switch_commands() {
        let mut app = dummy_app(Mode::Agent);
        app.run_command("/battle");
        assert_eq!(app.cfg.mode, Mode::Battle);
        app.run_command("/direct");
        assert_eq!(app.cfg.mode, Mode::Direct);
        app.run_command("/side");
        assert_eq!(app.cfg.mode, Mode::Side);
        app.run_command("/mode agent");
        assert_eq!(app.cfg.mode, Mode::Agent);
        app.run_command("/mode nonsense");
        assert_eq!(app.cfg.mode, Mode::Agent);
    }

    #[test]
    fn models_command_sets_two_panes() {
        let mut app = dummy_app(Mode::Battle);
        app.run_command("/models gpt-5,claude-4,extra");
        assert_eq!(app.cfg.models, vec!["gpt-5".to_string(), "claude-4".to_string()]);
        assert_eq!(app.model_label(), "gpt-5 vs claude-4");
    }

    #[test]
    fn tab_completion() {
        let mut app = dummy_app(Mode::Agent);
        app.set_input("/ba".to_string());
        app.handle_key(Key::Tab);
        assert_eq!(app.input, "/battle ");
    }

    #[test]
    fn tab_completion_common_prefix() {
        let mut app = dummy_app(Mode::Agent);
        app.set_input("/re".to_string());
        app.handle_key(Key::Tab);
        // /reasoning /repo /repos /resume /retry → 公共前缀 "/re"
        assert_eq!(app.input, "/re");
        assert!(app.notice.is_some());
    }

    #[test]
    fn scroll_up_stops_following_and_bottom_resumes() {
        let mut app = dummy_app(Mode::Agent);
        app.view_h = 10;
        app.follow = true;
        app.scroll_page(true);
        assert!(!app.follow);
        app.anchor(100);
        assert!(app.scroll_top < 90);
        // 一直往下滚到底，恢复跟随
        for _ in 0..20 {
            app.scroll_page(false);
        }
        app.anchor(100);
        assert!(app.follow);
        assert_eq!(app.scroll_top, 90);
    }

    #[test]
    fn follow_anchor_tracks_bottom() {
        let mut app = dummy_app(Mode::Agent);
        app.view_h = 5;
        app.anchor(40);
        assert_eq!(app.scroll_top, 35);
        assert!(app.follow);
    }

    #[test]
    fn reasoning_and_tool_blocks_accumulate() {
        let mut app = dummy_app(Mode::Agent);
        app.stream = Some(StreamState {
            id: 7,
            started: Instant::now(),
            verifying: false,
            got_output: false,
        });
        app.on_stream(7, StreamEvent::Reasoning("第一段".into()));
        app.on_stream(7, StreamEvent::Reasoning("第二段".into()));
        assert_eq!(app.blocks.len(), 1);
        app.on_stream(
            7,
            StreamEvent::Tool {
                name: "read_file".into(),
                input: "{\"path\":\"a.rs\"}".into(),
            },
        );
        app.on_stream(7, StreamEvent::Text("正文".into()));
        assert_eq!(app.blocks.len(), 3);
        // 旧回合的残余要丢掉
        app.on_stream(6, StreamEvent::Text("不该出现".into()));
        assert_eq!(app.blocks.len(), 3);
    }

    #[test]
    fn stale_stream_end_is_ignored() {
        let mut app = dummy_app(Mode::Agent);
        app.stream = Some(StreamState {
            id: 3,
            started: Instant::now(),
            verifying: false,
            got_output: false,
        });
        app.on_stream_end(2, Ok(()));
        assert!(app.stream.is_some());
        app.on_stream_end(3, Ok(()));
        assert!(app.stream.is_none());
    }

    #[test]
    fn errors_are_chinese_and_pop_user_msg() {
        let mut app = dummy_app(Mode::Agent);
        app.log.push(ChatMsg::user("问题"));
        app.stream = Some(StreamState {
            id: 1,
            started: Instant::now(),
            verifying: false,
            got_output: false,
        });
        app.on_stream_end(1, Err(BridgeError::Connect("x".into())));
        assert!(app.log.is_empty());
        match app.blocks.last() {
            Some(Block::Error { text }) => assert!(text.contains("连不上桥")),
            _ => panic!("应当有一条错误块"),
        }
    }

    #[test]
    fn relative_time_formatting() {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        assert_eq!(format_relative(now - 5_000), "刚刚");
        assert_eq!(format_relative(now - 120_000), "2 分钟前");
        assert_eq!(format_relative(now - 7_200_000), "2 小时前");
        assert_eq!(format_relative(now - 3 * 86_400_000), "3 天前");
        assert_eq!(format_relative(0), "时间未知");
    }

    #[test]
    fn unknown_command_gives_notice() {
        let mut app = dummy_app(Mode::Agent);
        app.run_command("/nope");
        assert!(app.notice.as_ref().map(|(t, _)| t.contains("没有这个命令")).unwrap_or(false));
    }
}
