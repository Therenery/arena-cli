//! 桥的错误翻译成中文人话。
//!
//! 原则：每条错误都要说清「发生了什么」和「现在该干什么」，
//! 不让用户看到 `error sending request` 这种话，更不让程序 panic。

use crate::httpc::HttpError;
use crate::json;

#[derive(Debug, Clone)]
pub enum BridgeError {
    /// 需要人工过人机验证（Cloudflare）
    NeedVerify,
    /// 桥的队列满 / 同一会话节奏太快
    QueueFull(String),
    /// ARB_TOKEN 不匹配
    Auth(String),
    /// Chrome/CDP 没起来
    ChromeDown,
    /// 超时（含长时间无帧）
    Timeout(String),
    /// 连不上桥
    Connect(String),
    /// 其它 HTTP 错误
    Http { status: u16, message: String },
    /// 响应不符合预期
    Protocol(String),
    /// 用户主动中断
    Aborted,
}

impl BridgeError {
    /// 状态栏用的短标签
    pub fn short(&self) -> &'static str {
        match self {
            BridgeError::NeedVerify => "待验证",
            BridgeError::QueueFull(_) => "队列满",
            BridgeError::Auth(_) => "鉴权",
            BridgeError::ChromeDown => "无Chrome",
            BridgeError::Timeout(_) => "超时",
            BridgeError::Connect(_) => "断连",
            BridgeError::Http { .. } => "错误",
            BridgeError::Protocol(_) => "协议",
            BridgeError::Aborted => "已中断",
        }
    }

    /// 正文里展示的多行中文说明（含「该怎么办」）
    pub fn human(&self) -> String {
        match self {
            BridgeError::NeedVerify => "需要人工过人机验证。已把 Chrome 切到前台：在浏览器里点一下验证，\
完成后这次请求会自动重试（只重试一次）。"
                .to_string(),
            BridgeError::QueueFull(d) => format!(
                "桥的队列满了：最多同时 4 个任务，同一会话两次提问至少间隔 3 秒。\n\
    等几秒再发，或者先停掉并发的客户端。{}",
                detail_line(d)
            ),
            BridgeError::Auth(d) => format!(
                "鉴权失败：桥设了 ARB_TOKEN，但客户端没带或对不上。\n\
    用 `--token <值>` 或环境变量 ARENA_TOKEN（也认 ARB_TOKEN）填成同一个值。{}",
                detail_line(d)
            ),
            BridgeError::ChromeDown => "Chrome 没在跑（9222 端口连不上）。\n\
    先启动它：`schtasks /run /tn chrome-arena`，或手动 \
`chrome.exe --remote-debugging-port=9222 --user-data-dir=<配置目录>`。"
                .to_string(),
            BridgeError::Timeout(d) => format!(
                "等太久没动静，判定超时。\n\
    桥那头可能卡在工具调用或页面上：去 Chrome 看一眼标签页是否在转圈；\
需要停就在 TUI 里按 Ctrl+C 中断。{}",
                detail_line(d)
            ),
            BridgeError::Connect(d) => format!(
                "连不上桥 {}\n\
    桥没启动？先跑 `node bridge/arena_bridge.js`（或 `schtasks /run /tn arena-bridge`），\
确认地址端口后再来；用 `arena-lite health` 可以快速验证。",
                if d.trim().is_empty() { "" } else { d }
            ),
            BridgeError::Http { status, message } => format!(
                "桥返回 HTTP {}：{}\n{}",
                status,
                message.trim(),
                hint_for_status(*status)
            ),
            BridgeError::Protocol(d) => {
                format!("响应看不懂（既不象 JSON 也不象 SSE）：{}", truncate(d, 300))
            }
            BridgeError::Aborted => "已中断。".to_string(),
        }
    }

    /// 是否值得自动重试一次（只有验证这一类）
    pub fn wants_verify(&self) -> bool {
        matches!(self, BridgeError::NeedVerify)
    }

    /// 从 HTTP 状态码 + 响应体猜具体原因
    pub fn from_status(status: u16, body: &str) -> BridgeError {
        let message = extract_message(body);
        let lower = message.to_lowercase();
        if status == 409
            || message.contains("NEED_VERIFY")
            || lower.contains("need_verify")
            || lower.contains("captcha")
        {
            return BridgeError::NeedVerify;
        }
        if status == 401 || status == 403 || lower.contains("unauthorized") {
            return BridgeError::Auth(message);
        }
        if status == 429 || lower.contains("queue full") || lower.contains("busy") {
            return BridgeError::QueueFull(message);
        }
        if status == 499 {
            return BridgeError::Aborted;
        }
        if lower.contains("chrome unavailable")
            || lower.contains("chrome not")
            || lower.contains("cdp timeout")
            || lower.contains("ws open failed")
        {
            return BridgeError::ChromeDown;
        }
        if status == 408 || status == 504 {
            return BridgeError::Timeout(message);
        }
        BridgeError::Http { status, message }
    }

    pub fn from_http(e: HttpError) -> BridgeError {
        match e {
            HttpError::Connect(d) => BridgeError::Connect(d),
            HttpError::Timeout(d) => BridgeError::Timeout(d),
            HttpError::Io(d) => BridgeError::Connect(d),
            HttpError::Protocol(d) => BridgeError::Protocol(d),
        }
    }
}

fn hint_for_status(status: u16) -> &'static str {
    match status {
        404 => "路径或会话名不对：确认桥版本够新（/history 需要 0.4 以上），会话名从 /health 的 sessions 里挑。",
        400 => "请求体被桥拒了：检查提示词是否为空、JSON 是否合规。",
        413 => "上传太大，桥只收 4MB 以内的请求体。",
        500 => "桥内部出错：看桥窗口的日志（arena_bridge.js 会打印 v1 error）。",
        _ => "看桥窗口的日志，里面会有具体原因。",
    }
}

fn detail_line(d: &str) -> String {
    let d = d.trim();
    if d.is_empty() {
        String::new()
    } else {
        format!("\n（桥的原话：{}）", truncate(d, 200))
    }
}

/// 从各种形状的错误体里抠出可读消息
pub fn extract_message(body: &str) -> String {
    let body = body.trim();
    if body.is_empty() {
        return "（空响应）".to_string();
    }
    if let Ok(v) = json::parse(body) {
        // {"error":{"message":"...","type":"..."}}
        if let Some(m) = v.str_at(&["error", "message"]) {
            return m.to_string();
        }
        // {"error":"..."}
        if let Some(m) = v.str_at(&["error"]) {
            return m.to_string();
        }
        // {"message":"..."}
        if let Some(m) = v.str_at(&["message"]) {
            return m.to_string();
        }
        // {"ok":false,"error":...} 已经是上面那种
        // {"choices":[{"delta":{"content":"..."}}]} 之类的流式错误帧
        if let Some(m) = v.str_at(&["choices"]) {
            return m.to_string();
        }
    }
    // 不是 JSON：可能是 SSE 里的 data: 行
    for line in body.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("data:") {
            let rest = rest.trim();
            if !rest.is_empty() && !rest.eq_ignore_ascii_case("[done]") {
                if let Ok(v) = json::parse(rest) {
                    if let Some(m) = v.str_at(&["error", "message"]) {
                        return m.to_string();
                    }
                    if let Some(m) = v.str_at(&["error"]) {
                        return m.to_string();
                    }
                }
                return truncate(rest, 300);
            }
        }
    }
    truncate(body, 300)
}

/// 按字符（不是字节）截断，避免把中文切成半个
pub fn truncate(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max_chars).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_verify_by_status_and_body() {
        assert!(matches!(
            BridgeError::from_status(409, r#"{"error":"NEED_VERIFY"}"#),
            BridgeError::NeedVerify
        ));
        assert!(matches!(
            BridgeError::from_status(500, r#"{"error":{"message":"NEED_VERIFY"}}"#),
            BridgeError::NeedVerify
        ));
        assert!(matches!(
            BridgeError::from_status(200, "data: {\"error\":{\"message\":\"NEED_VERIFY\"}}\n"),
            BridgeError::NeedVerify
        ));
    }

    #[test]
    fn classify_queue_full() {
        assert!(matches!(
            BridgeError::from_status(429, r#"{"error":{"message":"busy: queue full"}}"#),
            BridgeError::QueueFull(_)
        ));
        assert!(matches!(
            BridgeError::from_status(500, r#"{"error":"busy: queue full"}"#),
            BridgeError::QueueFull(_)
        ));
    }

    #[test]
    fn classify_auth_and_chrome() {
        assert!(matches!(
            BridgeError::from_status(
                401,
                r#"{"error":{"message":"unauthorized: set Authorization: Bearer $ARB_TOKEN"}}"#
            ),
            BridgeError::Auth(_)
        ));
        assert!(matches!(
            BridgeError::from_status(500, r#"{"error":"chrome unavailable"}"#),
            BridgeError::ChromeDown
        ));
    }

    #[test]
    fn human_text_is_chinese_and_has_advice() {
        let e = BridgeError::Connect("127.0.0.1:17892".into());
        let h = e.human();
        assert!(h.contains("连不上桥"));
        assert!(h.contains("arena_bridge.js"));
        assert!(BridgeError::NeedVerify.human().contains("验证"));
        assert!(BridgeError::QueueFull(String::new()).human().contains("3 秒"));
        assert!(BridgeError::Auth(String::new()).human().contains("ARENA_TOKEN"));
        assert!(BridgeError::ChromeDown.human().contains("9222"));
        assert!(BridgeError::Timeout(String::new()).human().contains("超时"));
    }

    #[test]
    fn extract_message_shapes() {
        assert_eq!(
            extract_message(r#"{"error":{"message":"bad json","type":"x"}}"#),
            "bad json"
        );
        assert_eq!(extract_message(r#"{"error":"empty prompt"}"#), "empty prompt");
        assert_eq!(extract_message("plain text"), "plain text");
        assert_eq!(extract_message(""), "（空响应）");
        assert!(extract_message("data: {\"error\":\"NEED_VERIFY\"}").contains("NEED_VERIFY"));
    }

    #[test]
    fn truncate_by_chars() {
        assert_eq!(truncate("你好世界", 2), "你好…");
        assert_eq!(truncate("abc", 10), "abc");
    }

    #[test]
    fn http_errors_map() {
        assert!(matches!(
            BridgeError::from_http(HttpError::Timeout("x".into())),
            BridgeError::Timeout(_)
        ));
        assert!(matches!(
            BridgeError::from_http(HttpError::Connect("x".into())),
            BridgeError::Connect(_)
        ));
    }
}
