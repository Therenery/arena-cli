//! arena_bridge 的类型化客户端。
//!
//! 覆盖桥接的全部对外接口：
//!   GET  /health /debug /models /v1/models /repos /repo /new /pr
//!   POST /v1/chat/completions（流式 SSE + 非流式）
//!   POST /ask /upload /battle /vote

use crate::http::{self, Endpoint};
use crate::json::Json;
use std::time::Duration;

pub const DEFAULT_BASE: &str = "http://127.0.0.1:17892";

#[derive(Clone)]
pub struct Bridge {
    pub ep: Endpoint,
    pub token: Option<String>,
    pub timeout: Duration,
}

/// 流式对话过程中上抛的事件（对应 chat.completion.chunk 里的 delta）。
#[derive(Debug, Clone)]
pub enum StreamEvent {
    Text(String),
    Reasoning(String),
    Done,
    Error(String),
}

#[derive(Clone)]
pub struct ChatRequest {
    pub model: String,
    pub session: String,
    /// 页面模式：agent / direct / battle / side
    pub mode: String,
    /// 本轮前清空服务端历史（arena_new）
    pub fresh: bool,
    /// 完整对话记录（role: user/assistant），桥接只取最后一条提问，其余用于回放
    pub messages: Vec<(String, String)>,
}

#[derive(Debug, Default, Clone)]
pub struct ChatReply {
    pub text: String,
    pub reasoning: String,
    pub routed: Option<String>,
    pub tools: Vec<String>,
    pub truncated: bool,
}

impl Bridge {
    pub fn new(base_url: &str, token: Option<String>, timeout_secs: u64) -> Result<Bridge, String> {
        Ok(Bridge {
            ep: http::parse_base_url(base_url)?,
            token,
            timeout: Duration::from_secs(timeout_secs.max(5)),
        })
    }

    fn auth_headers(&self) -> Vec<(String, String)> {
        let mut h = Vec::new();
        if let Some(t) = &self.token {
            h.push(("Authorization".to_string(), format!("Bearer {}", t)));
        }
        h.push(("User-Agent".to_string(), format!("arena-lite/{}", env!("CARGO_PKG_VERSION"))));
        h
    }

    fn send_req(
        &self,
        method: &str,
        path: &str,
        body: Option<&Json>,
    ) -> Result<http::Response, String> {
        let b = body.map(|j| j.to_string().into_bytes());
        let mut headers = self.auth_headers();
        http::request(&self.ep, method, path, &headers, b.as_deref(), self.timeout).map_err(|e| {
            format!(
                "连不上桥接 http://{}:{}{} （{}）— 桥接服务起来了吗？可先跑: curl http://{}:{}/health",
                self.ep.host, self.ep.port, self.ep.prefix, e, self.ep.host, self.ep.port
            )
        })
    }

    pub fn get_json(&self, path: &str) -> Result<(u16, Json), String> {
        let res = self.send_req("GET", path, None)?;
        let raw = res.body.read_to_end().map_err(|e| e.to_string())?;
        let text = String::from_utf8_lossy(&raw).into_owned();
        let j = Json::parse(&text).unwrap_or_else(|_| Json::Str(text.clone()));
        Ok((res.status, j))
    }

    pub fn post_json(&self, path: &str, body: &Json) -> Result<(u16, Json), String> {
        let res = self.send_req("POST", path, Some(body))?;
        let raw = res.body.read_to_end().map_err(|e| e.to_string())?;
        let text = String::from_utf8_lossy(&raw).into_owned();
        let j = Json::parse(&text).unwrap_or_else(|_| Json::Str(text.clone()));
        Ok((res.status, j))
    }

    fn check(status: u16, j: &Json) -> Result<Json, String> {
        if (200..300).contains(&status) {
            return Ok(j.clone());
        }
        let msg = j
            .get("error")
            .and_then(|e| {
                e.as_str()
                    .map(|s| s.to_string())
                    .or_else(|| e.get("message").and_then(|m| m.as_str()).map(|s| s.to_string()))
            })
            .unwrap_or_else(|| format!("HTTP {}", status));
        Err(friendly(&msg, status))
    }

    // ---------- 运维接口 ----------

    pub fn health(&self) -> Result<Json, String> {
        let (s, j) = self.get_json("/health")?;
        Self::check(s, &j)
    }

    pub fn debug(&self, session: &str) -> Result<Json, String> {
        let (s, j) = self.get_json(&format!("/debug?session={}", urlencode(session)))?;
        Self::check(s, &j)
    }

    /// /v1/models —— OpenAI 兼容模型表（arena-agent / arena-direct）
    pub fn models_v1(&self) -> Result<Vec<String>, String> {
        let (s, j) = self.get_json("/v1/models")?;
        let j = Self::check(s, &j)?;
        let mut out = Vec::new();
        if let Some(arr) = j.get("data").and_then(|d| d.as_arr()) {
            for m in arr {
                if let Some(id) = m.get("id").and_then(|i| i.as_str()) {
                    out.push(id.to_string());
                }
            }
        }
        Ok(out)
    }

    /// /models —— 路由模型（auto / max）
    pub fn models_routing(&self) -> Result<Vec<String>, String> {
        let (s, j) = self.get_json("/models")?;
        let j = Self::check(s, &j)?;
        let mut out = Vec::new();
        if let Some(arr) = j.get("models").and_then(|m| m.as_arr()) {
            for m in arr {
                if let Some(x) = m.as_str() {
                    out.push(x.to_string());
                }
            }
        }
        Ok(out)
    }

    pub fn reset(&self, session: &str) -> Result<(), String> {
        let (s, j) = self.get_json(&format!("/new?session={}", urlencode(session)))?;
        Self::check(s, &j)?;
        Ok(())
    }

    pub fn repos(&self, session: &str, cached: bool) -> Result<Json, String> {
        let mut p = format!("/repos?session={}", urlencode(session));
        if cached {
            p.push_str("&cached=1");
        }
        let (s, j) = self.get_json(&p)?;
        Self::check(s, &j)
    }

    pub fn switch_repo(&self, session: &str, name: &str) -> Result<Json, String> {
        let (s, j) = self.get_json(&format!(
            "/repo?session={}&name={}",
            urlencode(session),
            urlencode(name)
        ))?;
        Self::check(s, &j)
    }

    pub fn upload(&self, session: &str, files: &[String]) -> Result<Json, String> {
        let body = Json::obj(vec![
            ("session", Json::str(session)),
            (
                "files",
                Json::Arr(files.iter().map(|f| Json::str(f.clone())).collect()),
            ),
        ]);
        let (s, j) = self.post_json("/upload", &body)?;
        Self::check(s, &j)
    }

    pub fn pr(&self, repo: &str, n: Option<&str>) -> Result<Json, String> {
        let mut p = format!("/pr?repo={}", urlencode(repo));
        if let Some(n) = n {
            p.push_str(&format!("&n={}", urlencode(n)));
        }
        let (s, j) = self.get_json(&p)?;
        Self::check(s, &j)
    }

    pub fn battle(&self, session: &str, prompt: &str, mode: &str, models: &[String]) -> Result<Json, String> {
        let body = Json::obj(vec![
            ("session", Json::str(session)),
            ("prompt", Json::str(prompt)),
            ("mode", Json::str(mode)),
            (
                "models",
                Json::Arr(models.iter().map(|m| Json::str(m.clone())).collect()),
            ),
        ]);
        let (s, j) = self.post_json("/battle", &body)?;
        Self::check(s, &j)
    }

    pub fn vote(&self, session: &str, choice: &str) -> Result<Json, String> {
        let body = Json::obj(vec![
            ("session", Json::str(session)),
            ("choice", Json::str(choice)),
        ]);
        let (s, j) = self.post_json("/vote", &body)?;
        Self::check(s, &j)
    }

    // ---------- 对话接口 ----------

    fn chat_body(&self, req: &ChatRequest, stream: bool) -> Json {
        let msgs: Vec<Json> = req
            .messages
            .iter()
            .map(|(r, c)| Json::obj(vec![("role", Json::str(r.clone())), ("content", Json::str(c.clone()))]))
            .collect();
        let mut b = vec![
            ("model", Json::str(req.model.clone())),
            ("user", Json::str(req.session.clone())),
            ("messages", Json::Arr(msgs)),
            ("stream", Json::Bool(stream)),
            (
                "metadata",
                Json::obj(vec![("mode", Json::str(req.mode.clone()))]),
            ),
        ];
        if req.fresh {
            b.push(("arena_new", Json::Bool(true)));
        }
        Json::Obj(b.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
    }

    /// 非流式单轮（等桥接跑完才返回，适合脚本）。
    pub fn chat_once(&self, req: &ChatRequest) -> Result<ChatReply, String> {
        let body = self.chat_body(req, false);
        let (status, j) = self.post_json("/v1/chat/completions", &body)?;
        let j = Self::check(status, &j)?;
        let mut r = ChatReply::default();
        if let Some(choices) = j.get("choices").and_then(|c| c.as_arr()) {
            if let Some(ch) = choices.first() {
                if let Some(m) = ch.get("message") {
                    if let Some(t) = m.get("content").and_then(|x| x.as_str()) {
                        r.text = t.to_string();
                    }
                }
            }
        }
        if let Some(a) = j.get("arena") {
            if let Some(t) = a.get("reasoning").and_then(|x| x.as_str()) {
                r.reasoning = t.to_string();
            }
            if let Some(t) = a.get("routed").and_then(|x| x.as_str()) {
                r.routed = Some(t.to_string());
            }
            if let Some(t) = a.get("truncated").and_then(|x| x.as_bool()) {
                r.truncated = t;
            }
            if let Some(arr) = a.get("tools").and_then(|x| x.as_arr()) {
                for t in arr {
                    r.tools.push(t.to_string());
                }
            }
        }
        Ok(r)
    }

    /// 流式对话：逐 chunk 调 `on_ev`，直到流结束（Done）或出错。
    /// 该函数会阻塞，TUI 里请在线程里跑。
    pub fn chat_stream<F: Fn(StreamEvent)>(&self, req: &ChatRequest, on_ev: F) -> Result<(), String> {
        let body = self.chat_body(req, true);
        let res = self.send_req("POST", "/v1/chat/completions", Some(&body))?;
        if res.status != 200 {
            let raw = res.body.read_to_end().map_err(|e| e.to_string())?;
            let t = String::from_utf8_lossy(&raw).into_owned();
            let msg = Json::parse(&t)
                .ok()
                .and_then(|j| {
                    j.get("error")
                        .and_then(|e| e.get("message").and_then(|m| m.as_str()).map(|s| s.to_string()))
                        .or_else(|| j.get("error").and_then(|e| e.as_str()).map(|s| s.to_string()))
                })
                .unwrap_or(t);
            return Err(friendly(&msg, res.status));
        }
        let mut br = res.body;
        loop {
            let line = match br.read_line() {
                Ok(Some(l)) => l,
                Ok(None) => break,
                Err(e) => return Err(format!("读取流中断: {}", e)),
            };
            let line = line.trim_end();
            if line.is_empty() || line.starts_with(':') {
                continue; // SSE 空行 / `: ping` 心跳
            }
            let data = match line.strip_prefix("data:") {
                Some(d) => d.trim_start(),
                None => continue,
            };
            if data == "[DONE]" {
                on_ev(StreamEvent::Done);
                break;
            }
            let j = match Json::parse(data) {
                Ok(j) => j,
                Err(_) => continue,
            };
            if let Some(err) = j.get("error") {
                let msg = err
                    .get("message")
                    .and_then(|m| m.as_str())
                    .or_else(|| err.as_str())
                    .unwrap_or("upstream error");
                on_ev(StreamEvent::Error(friendly(msg, 500)));
                on_ev(StreamEvent::Done);
                break;
            }
            if let Some(choices) = j.get("choices").and_then(|c| c.as_arr()) {
                if let Some(ch) = choices.first() {
                    if let Some(delta) = ch.get("delta") {
                        if let Some(c) = delta.get("content").and_then(|x| x.as_str()) {
                            if !c.is_empty() {
                                on_ev(StreamEvent::Text(c.to_string()));
                            }
                        }
                        if let Some(rc) = delta.get("reasoning_content").and_then(|x| x.as_str()) {
                            if !rc.is_empty() {
                                on_ev(StreamEvent::Reasoning(rc.to_string()));
                            }
                        }
                    }
                    if let Some(f) = ch.get("finish_reason") {
                        if !matches!(f, Json::Null) {
                            on_ev(StreamEvent::Done);
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

/// 把桥接的英文错误翻译成人话。
pub fn friendly(msg: &str, status: u16) -> String {
    let m = msg.trim();
    if m.contains("NEED_VERIFY") || status == 409 {
        return "Arena 弹了人机验证：去电脑上的 Chrome 标签页点一下再重试".to_string();
    }
    if m.contains("busy") || status == 429 {
        return "桥接队列已满（同时只跑一个任务），等前一个跑完再发".to_string();
    }
    if m.contains("unauthorized") || status == 401 {
        return "鉴权失败：检查 --token / $ARENA_TOKEN 是否与桥接端 ARB_TOKEN 一致".to_string();
    }
    if m.contains("chrome") {
        return "Chrome 没起来：在电脑上确认 chrome-arena 计划任务（CDP 9222），稍等几秒再试".to_string();
    }
    if m.contains("no session") || status == 404 {
        return "会话不存在（可能已被 /new 清掉）".to_string();
    }
    if m.contains("not found") || status == 404 {
        return "会话不存在".to_string();
    }
    m.to_string()
}

pub fn fmt_health(j: &Json) -> String {
    let ver = j.get("version").and_then(|v| v.as_str()).unwrap_or("?");
    let up = j.get("uptime").and_then(|v| v.as_f64()).unwrap_or(0.0) as u64;
    let chrome = j.get("chrome").and_then(|v| v.as_bool()).unwrap_or(false);
    let queue = j.get("queue").and_then(|v| v.as_f64()).unwrap_or(0.0) as u64;
    let working = j.get("working").and_then(|v| v.as_bool()).unwrap_or(false);
    let auth = j.get("auth").and_then(|v| v.as_bool()).unwrap_or(false);
    let sessions = j
        .get("sessions")
        .and_then(|v| v.as_arr())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default();
    let maxq = j
        .get("limits")
        .and_then(|l| l.get("maxQueue"))
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0) as u64;
    format!(
        "bridge v{} │ 运行 {}s │ chrome {} │ 队列 {}/{} │ {} │ auth {} │ 会话 [{}]",
        ver,
        up,
        if chrome { "✓" } else { "✗" },
        queue,
        maxq,
        if working { "工作中" } else { "空闲" },
        if auth { "on" } else { "off" },
        sessions
    )
}

pub fn urlencode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_encoding() {
        assert_eq!(urlencode("Therenery/Backrooms"), "Therenery%2FBackrooms");
        assert_eq!(urlencode("a b"), "a%20b");
        assert_eq!(urlencode("中文"), "%E4%B8%AD%E6%96%87");
    }

    #[test]
    fn friendly_maps() {
        assert!(friendly("NEED_VERIFY", 500).contains("人机验证"));
        assert!(friendly("busy: queue full", 429).contains("队列"));
        assert!(friendly("unauthorized", 401).contains("鉴权"));
        assert_eq!(friendly("some other", 500), "some other");
    }
}
