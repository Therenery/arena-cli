//! 对接 arena_bridge 的客户端。所有请求都走这里，UI 只认结构化事件。

use std::time::Duration;

use crate::err::BridgeError;
use crate::httpc::{self, Base, Request};
use crate::json::{self, Value};
use crate::mode::Mode;
use crate::sse::{self, SseParser, Utf8Decoder};
use crate::urlenc;

/// 一条对话消息
#[derive(Debug, Clone, PartialEq)]
pub struct Msg {
    pub role: String,
    pub content: String,
}

impl Msg {
    pub fn user(text: impl Into<String>) -> Msg {
        Msg {
            role: "user".to_string(),
            content: text.into(),
        }
    }

    pub fn assistant(text: impl Into<String>) -> Msg {
        Msg {
            role: "assistant".to_string(),
            content: text.into(),
        }
    }
}

/// 流式事件
#[derive(Debug, Clone, PartialEq)]
pub enum StreamEvent {
    Text(String),
    Reasoning(String),
    Tool { name: String, input: String },
}

/// 一次提问的参数
#[derive(Debug, Clone)]
pub struct ChatOpts {
    pub mode: Mode,
    pub models: Vec<String>,
    /// 覆盖 model 字段；None 就用模式默认值
    pub model: Option<String>,
    pub session: String,
}

impl ChatOpts {
    pub fn new(mode: Mode, session: impl Into<String>) -> ChatOpts {
        ChatOpts {
            mode,
            models: Vec::new(),
            model: None,
            session: session.into(),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Health {
    pub ok: bool,
    pub chrome: bool,
    pub version: String,
    pub uptime: i64,
    pub sessions: Vec<String>,
    pub queue: i64,
    pub working: bool,
    pub max_queue: i64,
    pub min_gap_ms: i64,
    pub auth: bool,
}

impl Health {
    pub fn from_json(v: &Value) -> Health {
        Health {
            ok: v.bool_at(&["ok"]).unwrap_or(false),
            chrome: v.bool_at(&["chrome"]).unwrap_or(false),
            version: v.str_at(&["version"]).unwrap_or("").to_string(),
            uptime: v.i64_at(&["uptime"]).unwrap_or(0),
            sessions: v.str_arr_at(&["sessions"]),
            queue: v.i64_at(&["queue"]).unwrap_or(0),
            working: v.bool_at(&["working"]).unwrap_or(false),
            max_queue: v.i64_at(&["limits", "maxQueue"]).unwrap_or(4),
            min_gap_ms: v.i64_at(&["limits", "minGapMs"]).unwrap_or(3000),
            auth: v.bool_at(&["auth"]).unwrap_or(false),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct HistoryMsg {
    pub role: String,
    pub text: String,
}

#[derive(Debug, Clone, Default)]
pub struct History {
    pub session: String,
    pub messages: Vec<HistoryMsg>,
    /// 桥能提供就带上（毫秒时间戳），拿不到就是 None
    pub at: Option<i64>,
    pub turns: i64,
    /// 桥读 DOM 用的策略：role-attrs / bubble-align / alternate
    pub via: String,
}

pub struct Client {
    pub base: Base,
    pub token: Option<String>,
    /// 普通请求的读超时
    pub timeout: Duration,
    /// 流式请求的读超时（长任务靠 SSE 心跳续命，这里给得宽一点）
    pub stream_timeout: Duration,
}

impl Client {
    pub fn new(base: Base, token: Option<String>) -> Client {
        Client {
            base,
            token,
            timeout: Duration::from_secs(60),
            stream_timeout: Duration::from_secs(180),
        }
    }

    fn decorate<'a>(&self, req: Request<'a>) -> Request<'a> {
        match &self.token {
            Some(t) if !t.is_empty() => req.header("Authorization", &format!("Bearer {}", t)),
            _ => req,
        }
    }

    /// 发请求并确保拿到 2xx，否则翻译成 BridgeError
    fn call(&self, req: Request<'_>) -> Result<httpc::Response, BridgeError> {
        let req = self.decorate(req);
        let mut resp = httpc::send(&self.base, req).map_err(|e| match BridgeError::from_http(e) {
            // 连不上时把地址摆出来，人话才完整
            BridgeError::Connect(d) => {
                BridgeError::Connect(format!("{}（{}）", self.base.display(), d))
            }
            other => other,
        })?;
        if !resp.is_success() {
            let body = resp.read_text(1024 * 1024).unwrap_or_default();
            return Err(BridgeError::from_status(resp.status, &body));
        }
        Ok(resp)
    }

    fn get_json(&self, path: &str) -> Result<Value, BridgeError> {
        let mut resp = self.call(Request::new("GET", path).read_timeout(self.timeout))?;
        let text = resp
            .read_text(8 * 1024 * 1024)
            .map_err(BridgeError::from_http)?;
        json::parse(&text).map_err(|e| BridgeError::Protocol(format!("JSON 解析失败：{}", e)))
    }

    fn post_json(&self, path: &str, body: String) -> Result<Value, BridgeError> {
        let req = Request::new("POST", path)
            .json_body(body)
            .read_timeout(self.timeout);
        let mut resp = self.call(req)?;
        let text = resp
            .read_text(8 * 1024 * 1024)
            .map_err(BridgeError::from_http)?;
        json::parse(&text).map_err(|e| BridgeError::Protocol(format!("JSON 解析失败：{}", e)))
    }

    /* ------------------------------ 只读接口 ------------------------------ */

    pub fn health(&self) -> Result<Health, BridgeError> {
        Ok(Health::from_json(&self.get_json("/health")?))
    }

    pub fn models(&self) -> Result<Vec<String>, BridgeError> {
        let v = self.get_json("/models")?;
        let mut out: Vec<String> = Vec::new();
        for m in v.str_arr_at(&["models"]) {
            out.push(m);
        }
        if out.is_empty() {
            for m in v.str_arr_at(&["data"]) {
                out.push(m);
            }
            // /v1/models 是 [{id:...}]
            if let Some(arr) = v.arr_at(&["data"]) {
                for item in arr {
                    if let Some(id) = item.str_at(&["id"]) {
                        out.push(id.to_string());
                    }
                }
            }
        }
        out.dedup();
        Ok(out)
    }

    pub fn repos(&self) -> Result<(String, Vec<String>), BridgeError> {
        let v = self.get_json("/repos?cached=1")?;
        let current = v.str_at(&["repo"]).unwrap_or("").to_string();
        let mut list = Vec::new();
        if let Some(arr) = v.arr_at(&["repos"]) {
            for item in arr {
                match item.as_str() {
                    Some(s) => list.push(s.to_string()),
                    None => {
                        if let Some(n) = item.str_at(&["name"]) {
                            list.push(n.to_string());
                        }
                    }
                }
            }
        }
        Ok((current, list))
    }

    /// 切仓库（等于开新对话）
    pub fn switch_repo(&self, name: &str, session: &str) -> Result<String, BridgeError> {
        let path = urlenc::with_query(
            "/repo",
            &[("session", session), ("name", name), ("cached", "0")],
        );
        let mut resp = self.call(Request::new("GET", &path).read_timeout(Duration::from_secs(240)))?;
        let text = resp
            .read_text(1024 * 1024)
            .map_err(BridgeError::from_http)?;
        Ok(text)
    }

    /// 清空服务端会话
    pub fn new_session(&self, session: &str) -> Result<(), BridgeError> {
        let path = urlenc::with_query("/new", &[("session", session)]);
        self.get_json(&path)?;
        Ok(())
    }

    /// 读网页端历史（桥的 GET /history，只读）
    pub fn history(&self, session: &str) -> Result<History, BridgeError> {
        let path = urlenc::with_query("/history", &[("session", session)]);
        let v = self.get_json(&path)?;
        let ok = v.bool_at(&["ok"]).unwrap_or(false);
        if !ok {
            let msg = v
                .str_at(&["error"])
                .unwrap_or("桥读不到这个会话的 DOM")
                .to_string();
            return Err(BridgeError::Http {
                status: 200,
                message: format!(
                    "{}（会话 {} 没有打开，或页面结构变了）",
                    msg,
                    crate::err::truncate(session, 40)
                ),
            });
        }
        let mut messages = Vec::new();
        if let Some(arr) = v.arr_at(&["messages"]) {
            for m in arr {
                let role = m.str_at(&["role"]).unwrap_or("unknown").to_string();
                let text = m.str_at(&["text"]).unwrap_or("").to_string();
                if text.trim().is_empty() {
                    continue;
                }
                messages.push(HistoryMsg { role, text });
            }
        }
        Ok(History {
            session: v
                .str_at(&["session"])
                .unwrap_or(session)
                .to_string(),
            messages,
            at: v.i64_at(&["at"]),
            turns: v.i64_at(&["turns"]).unwrap_or(0),
            via: v.str_at(&["via"]).unwrap_or("").to_string(),
        })
    }

    /* ------------------------------ 写接口 ------------------------------ */

    pub fn vote(&self, choice: &str, session: &str) -> Result<String, BridgeError> {
        let body = format!(
            "{{\"session\":{},\"choice\":{}}}",
            json::quoted(session),
            json::quoted(choice)
        );
        let v = self.post_json("/vote", body)?;
        Ok(v.str_at(&["ok"]).map(|s| s.to_string()).unwrap_or_else(|| {
            if v.bool_at(&["ok"]).unwrap_or(false) {
                format!("投票已记录：{}", choice)
            } else {
                v.str_at(&["error"]).unwrap_or("投票失败").to_string()
            }
        }))
    }

    pub fn upload(&self, files: &[String], session: &str) -> Result<String, BridgeError> {
        let list = files
            .iter()
            .map(|f| json::quoted(f))
            .collect::<Vec<_>>()
            .join(",");
        let body = format!(
            "{{\"session\":{},\"files\":[{}]}}",
            json::quoted(session),
            list
        );
        let v = self.post_json("/upload", body)?;
        if v.bool_at(&["ok"]).unwrap_or(false) {
            let names = v.str_arr_at(&["attached"]);
            Ok(format!("已附加 {}", names.join(", ")))
        } else {
            Err(BridgeError::Http {
                status: 200,
                message: v
                    .str_at(&["error"])
                    .unwrap_or("上传失败")
                    .to_string(),
            })
        }
    }

    /// 流式提问。`sink` 返回 false 表示调用方要中断（Ctrl+C）。
    pub fn chat_stream(
        &self,
        messages: &[Msg],
        opts: &ChatOpts,
        sink: &mut dyn FnMut(StreamEvent) -> bool,
    ) -> Result<(), BridgeError> {
        let body = build_chat_body(messages, opts);
        let req = Request::new("POST", "/v1/chat/completions")
            .json_body(body)
            .header("Accept", "text/event-stream")
            .read_timeout(self.stream_timeout);
        let mut resp = self.call(req)?;

        let mut dec = Utf8Decoder::new();
        let mut parser = SseParser::new();
        let mut splitter = ReasoningSplitter::new();
        let mut buf = [0u8; 16 * 1024];
        let mut finished = false;

        loop {
            let n = match resp.body.read_some(&mut buf) {
                Ok(n) => n,
                Err(e) => return Err(BridgeError::from_http(e)),
            };
            if n == 0 {
                break;
            }
            let text = dec.push(&buf[..n]);
            if text.is_empty() {
                continue;
            }
            for ev in parser.push(&text) {
                match handle_event(&ev.data, &mut splitter, sink)? {
                    Dispatch::Done => {
                        finished = true;
                    }
                    Dispatch::Abort => return Err(BridgeError::Aborted),
                    Dispatch::Continue => {}
                }
                if finished {
                    break;
                }
            }
            if finished {
                break;
            }
        }
        for ev in parser.finish() {
            if handle_event(&ev.data, &mut splitter, sink)? == Dispatch::Abort {
                return Err(BridgeError::Aborted);
            }
        }
        splitter.flush(sink);
        Ok(())
    }
}

/* --------------------------- 请求体构造 --------------------------- */

pub fn build_chat_body(messages: &[Msg], opts: &ChatOpts) -> String {
    let mut msgs = String::from("[");
    for (i, m) in messages.iter().enumerate() {
        if i > 0 {
            msgs.push(',');
        }
        msgs.push_str(&format!(
            "{{\"role\":{},\"content\":{}}}",
            json::quoted(&m.role),
            json::quoted(&m.content)
        ));
    }
    msgs.push(']');

    let models = opts
        .models
        .iter()
        .filter(|m| !m.trim().is_empty())
        .take(2)
        .map(|m| json::quoted(m.trim()))
        .collect::<Vec<_>>()
        .join(",");

    let model = opts
        .model
        .clone()
        .filter(|m| !m.trim().is_empty())
        .unwrap_or_else(|| opts.mode.default_model().to_string());

    format!(
        "{{\"model\":{},\"messages\":{},\"stream\":true,\"user\":{},\
\"metadata\":{{\"mode\":{},\"models\":[{}]}}}}",
        json::quoted(&model),
        msgs,
        json::quoted(&opts.session),
        json::quoted(opts.mode.as_str()),
        models
    )
}

/* --------------------------- 事件分发 --------------------------- */

#[derive(PartialEq)]
enum Dispatch {
    Continue,
    Done,
    Abort,
}

fn handle_event(
    data: &str,
    splitter: &mut ReasoningSplitter,
    sink: &mut dyn FnMut(StreamEvent) -> bool,
) -> Result<Dispatch, BridgeError> {
    let data = data.trim();
    if data.is_empty() {
        return Ok(Dispatch::Continue);
    }
    if sse::is_done(data) {
        return Ok(Dispatch::Done);
    }
    let v = json::parse(data).map_err(|e| BridgeError::Protocol(format!("{}：{}", e, data)))?;

    // 流内错误帧：{"error":{"message":...}}
    if v.get("error").is_some() {
        let msg = crate::err::extract_message(data);
        return match BridgeError::from_status(200, &msg) {
            // 200 走的兜底分支会把状态码弄丢，这里直接按消息特征判
            _ => Err(classify_stream_error(&msg)),
        };
    }

    if let Some(choices) = v.arr_at(&["choices"]) {
        for ch in choices {
            let Some(delta) = ch.get("delta") else {
                continue;
            };
            if let Some(text) = delta.str_at(&["content"]) {
                if !text.is_empty() {
                    if !sink(StreamEvent::Text(text.to_string())) {
                        return Ok(Dispatch::Abort);
                    }
                }
            }
            // 桥把工具事件塞在 reasoning_content 里（"· name {json}" 行）
            if let Some(text) = delta.str_at(&["reasoning_content"]) {
                if !text.is_empty() {
                    let mut events = Vec::new();
                    splitter.push(text, &mut events);
                    for ev in events {
                        if !sink(ev) {
                            return Ok(Dispatch::Abort);
                        }
                    }
                }
            }
            // 未来若桥改用标准 tool_calls，这里能直接接住
            if let Some(calls) = delta.arr_at(&["tool_calls"]) {
                for call in calls {
                    let name = call
                        .str_at(&["function", "name"])
                        .unwrap_or("tool")
                        .to_string();
                    let input = call
                        .str_at(&["function", "arguments"])
                        .unwrap_or("")
                        .to_string();
                    if !sink(StreamEvent::Tool { name, input }) {
                        return Ok(Dispatch::Abort);
                    }
                }
            }
        }
    }
    // 只带 finish_reason 的收尾帧到这里就是「无事发生」，正常
    Ok(Dispatch::Continue)
}

fn classify_stream_error(msg: &str) -> BridgeError {
    if msg.contains("NEED_VERIFY") {
        BridgeError::NeedVerify
    } else {
        BridgeError::from_status(500, msg)
    }
}

/* ------------------------- 思考/工具行拆分 ------------------------- */

/// 桥把工具调用混在 reasoning_content 里，形如：
/// `\n· read_file {"path":"src/main.rs"}\n`
/// 这里按行把它拆回 Tool 事件，其余照旧当思考内容。
pub struct ReasoningSplitter {
    buf: String,
    at_line_start: bool,
}

impl Default for ReasoningSplitter {
    fn default() -> Self {
        ReasoningSplitter::new()
    }
}

impl ReasoningSplitter {
    pub fn new() -> Self {
        ReasoningSplitter {
            buf: String::new(),
            at_line_start: true,
        }
    }

    pub fn push(&mut self, chunk: &str, out: &mut Vec<StreamEvent>) {
        if chunk.is_empty() {
            return;
        }
        self.buf.push_str(chunk);
        loop {
            if self.buf.is_empty() {
                return;
            }
            if self.at_line_start {
                // 桥在工具行前面补了一个空行，这里把它吃掉，别让思考区多出空行
                if self.buf.starts_with('\n') {
                    let stripped = self.buf.trim_start_matches('\n');
                    if stripped.is_empty() {
                        return; // 只有换行，等下一片再判断
                    }
                    if stripped.starts_with('·') {
                        let consumed = self.buf.len() - stripped.len();
                        self.buf.drain(..consumed);
                        if self.buf.is_empty() {
                            return;
                        }
                    }
                }
                let starts_marker = self.buf.starts_with('·');
                if starts_marker {
                    match self.buf.find('\n') {
                        Some(nl) => {
                            let line = self.buf[..nl].to_string();
                            let rest = self.buf[nl + 1..].to_string();
                            out.push(match parse_tool_line(&line) {
                                Some((name, input)) => StreamEvent::Tool { name, input },
                                None => StreamEvent::Reasoning(line),
                            });
                            self.buf = rest;
                            continue;
                        }
                        None => {
                            // 还没换行：可能是工具行的一半。太长了就不像工具行，先吐出去
                            if self.buf.chars().count() > 4000 {
                                let all = std::mem::take(&mut self.buf);
                                out.push(StreamEvent::Reasoning(all));
                                self.at_line_start = false;
                            }
                            return;
                        }
                    }
                }
                match self.buf.find('\n') {
                    Some(nl) => {
                        let line = self.buf[..=nl].to_string();
                        let rest = self.buf[nl + 1..].to_string();
                        out.push(StreamEvent::Reasoning(line));
                        self.buf = rest;
                        continue;
                    }
                    None => {
                        let all = std::mem::take(&mut self.buf);
                        out.push(StreamEvent::Reasoning(all));
                        self.at_line_start = false;
                        return;
                    }
                }
            } else {
                match self.buf.find('\n') {
                    Some(nl) => {
                        let line = self.buf[..=nl].to_string();
                        let rest = self.buf[nl + 1..].to_string();
                        out.push(StreamEvent::Reasoning(line));
                        self.buf = rest;
                        self.at_line_start = true;
                        continue;
                    }
                    None => {
                        let all = std::mem::take(&mut self.buf);
                        out.push(StreamEvent::Reasoning(all));
                        return;
                    }
                }
            }
        }
    }

    /// 流结束时把残留吐出去
    pub fn flush(&mut self, sink: &mut dyn FnMut(StreamEvent) -> bool) {
        if self.buf.is_empty() {
            return;
        }
        let rest = std::mem::take(&mut self.buf);
        if rest.trim().is_empty() {
            self.at_line_start = true;
            return;
        }
        let ev = match parse_tool_line(rest.trim_end_matches('\n')) {
            Some((name, input)) => StreamEvent::Tool { name, input },
            None => StreamEvent::Reasoning(rest),
        };
        let _ = sink(ev);
        self.at_line_start = true;
    }
}

/// `· name {json}` → (name, json)
fn parse_tool_line(line: &str) -> Option<(String, String)> {
    let line = line.trim_end_matches('\n').trim();
    let rest = line.strip_prefix('·')?;
    let rest = rest.trim_start();
    if rest.is_empty() {
        return None;
    }
    let mut parts = rest.splitn(2, ' ');
    let name = parts.next().unwrap_or("").trim().to_string();
    if name.is_empty() || name.chars().count() > 64 {
        return None;
    }
    // 工具名只允许字母数字和 _ - . ：避免把普通中文思考误判成工具
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.')
    {
        return None;
    }
    let input = parts.next().unwrap_or("").trim().to_string();
    Some((name, input))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_body_has_mode_and_models() {
        let mut opts = ChatOpts::new(Mode::Side, "main");
        opts.models = vec!["gpt".into(), "claude".into(), "extra".into()];
        let body = build_chat_body(&[Msg::user("你好")], &opts);
        let v = json::parse(&body).expect("body is json");
        assert_eq!(v.str_at(&["model"]), Some("arena-agent"));
        assert_eq!(v.str_at(&["user"]), Some("main"));
        assert_eq!(v.str_at(&["metadata", "mode"]), Some("side"));
        assert_eq!(v.str_arr_at(&["metadata", "models"]), vec!["gpt", "claude"]);
        assert_eq!(v.bool_at(&["stream"]), Some(true));
        assert_eq!(v.str_at(&["messages"]), None); // messages 是数组
        assert_eq!(
            v.arr_at(&["messages"])
                .and_then(|m| m.first())
                .and_then(|m| m.str_at(&["content"])),
            Some("你好")
        );
    }

    #[test]
    fn chat_body_escapes_quotes() {
        let opts = ChatOpts::new(Mode::Agent, "a\"b");
        let body = build_chat_body(&[Msg::user("他说\"你好\"\n换行")], &opts);
        let v = json::parse(&body).expect("body is json");
        assert_eq!(
            v.arr_at(&["messages"])
                .and_then(|m| m.first())
                .and_then(|m| m.str_at(&["content"])),
            Some("他说\"你好\"\n换行")
        );
        assert_eq!(v.str_at(&["user"]), Some("a\"b"));
    }

    #[test]
    fn splitter_keeps_plain_reasoning() {
        let mut s = ReasoningSplitter::new();
        let mut out = Vec::new();
        s.push("先看看目录结构", &mut out);
        s.push("，再决定读哪个文件。", &mut out);
        assert_eq!(
            out,
            vec![
                StreamEvent::Reasoning("先看看目录结构".into()),
                StreamEvent::Reasoning("，再决定读哪个文件。".into())
            ]
        );
    }

    #[test]
    fn splitter_extracts_tool_line() {
        let mut s = ReasoningSplitter::new();
        let mut out = Vec::new();
        s.push("\n· read_file {\"path\":\"src/main.rs\"}\n", &mut out);
        assert_eq!(
            out,
            vec![StreamEvent::Tool {
                name: "read_file".into(),
                input: "{\"path\":\"src/main.rs\"}".into()
            }]
        );
    }

    #[test]
    fn splitter_handles_tool_line_split_across_chunks() {
        let mut s = ReasoningSplitter::new();
        let mut out = Vec::new();
        s.push("\n· read_", &mut out);
        s.push("file {\"path\":\"a.rs\"}", &mut out);
        s.push("\n", &mut out);
        assert_eq!(
            out,
            vec![StreamEvent::Tool {
                name: "read_file".into(),
                input: "{\"path\":\"a.rs\"}".into()
            }]
        );
    }

    #[test]
    fn splitter_does_not_mistake_chinese_for_tool() {
        let mut s = ReasoningSplitter::new();
        let mut out = Vec::new();
        s.push("\n· 我觉得应该先读文件\n", &mut out);
        assert_eq!(
            out,
            vec![StreamEvent::Reasoning("· 我觉得应该先读文件".into())]
        );
    }

    #[test]
    fn splitter_mixed_reasoning_and_tools() {
        let mut s = ReasoningSplitter::new();
        let mut out = Vec::new();
        s.push("思考一\n· shell {\"cmd\":\"ls\"}\n思考二", &mut out);
        s.flush(&mut |e| {
            out.push(e);
            true
        });
        assert_eq!(
            out,
            vec![
                StreamEvent::Reasoning("思考一\n".into()),
                StreamEvent::Tool {
                    name: "shell".into(),
                    input: "{\"cmd\":\"ls\"}".into()
                },
                StreamEvent::Reasoning("思考二".into()),
            ]
        );
    }

    #[test]
    fn parse_tool_line_rejects_non_tools() {
        assert!(parse_tool_line("· ").is_none());
        assert!(parse_tool_line("hello").is_none());
        assert!(parse_tool_line("· 中文名 {}").is_none());
        assert_eq!(
            parse_tool_line("· read_file {}"),
            Some(("read_file".to_string(), "{}".to_string()))
        );
        assert_eq!(
            parse_tool_line("· fs.read  {\"a\":1}"),
            Some(("fs.read".to_string(), "{\"a\":1}".to_string()))
        );
    }

    #[test]
    fn health_parse() {
        let raw = r#"{"ok":true,"chrome":true,"version":"0.4.0","uptime":42,"sessions":["main","battle#x"],
            "queue":1,"working":true,"limits":{"maxQueue":4,"maxSessions":6,"minGapMs":3000,"maxPromptChars":6000},"auth":false}"#;
        let v = json::parse(raw).expect("health");
        let h = Health::from_json(&v);
        assert!(h.ok && h.chrome && h.working);
        assert_eq!(h.version, "0.4.0");
        assert_eq!(h.sessions, vec!["main".to_string(), "battle#x".to_string()]);
        assert_eq!(h.max_queue, 4);
        assert_eq!(h.min_gap_ms, 3000);
    }
}
