//! SSE 分帧 + 增量 UTF-8 解码。
//!
//! 桥发的是 `data: {...}\n\n` 和 `data: [DONE]\n\n`，中间偶尔夹心跳注释行。
//! 网络分片会把任意一帧切成两半（中文还会把一个字切成两个字节），
//! 所以这里两条都要处理：字节层的 UTF-8 拼接、文本层的行拼帧。

/// 增量 UTF-8 解码器：把网络分片里被切断的多字节字符接回来
#[derive(Default)]
pub struct Utf8Decoder {
    pending: Vec<u8>,
}

impl Utf8Decoder {
    pub fn new() -> Self {
        Utf8Decoder { pending: Vec::new() }
    }

    /// 喂一段字节，返回当前能完整解出的文本（不完整的前缀留在内部）
    pub fn push(&mut self, bytes: &[u8]) -> String {
        if bytes.is_empty() {
            return String::new();
        }
        self.pending.extend_from_slice(bytes);
        let mut out = String::new();
        loop {
            match std::str::from_utf8(&self.pending) {
                Ok(s) => {
                    out.push_str(s);
                    self.pending.clear();
                    break;
                }
                Err(e) => {
                    let valid = e.valid_up_to();
                    if valid > 0 {
                        if let Ok(s) = std::str::from_utf8(&self.pending[..valid]) {
                            out.push_str(s);
                        }
                        self.pending.drain(..valid);
                    }
                    match e.error_len() {
                        // 真的非法字节：丢掉一个，用替换字符顶替，别卡住整个流
                        Some(n) => {
                            out.push('\u{fffd}');
                            let n = n.min(self.pending.len()).max(1);
                            self.pending.drain(..n);
                        }
                        // 只是被切断：等下一片
                        None => break,
                    }
                }
            }
        }
        out
    }

    /// 收尾：把残留的半个字符按替换字符吐出
    pub fn finish(&mut self) -> String {
        if self.pending.is_empty() {
            return String::new();
        }
        let s = String::from_utf8_lossy(&self.pending).to_string();
        self.pending.clear();
        s
    }
}

/// SSE 事件
#[derive(Debug, Clone, PartialEq)]
pub struct Event {
    /// `event:` 字段（桥不用，但协议里有）
    pub name: String,
    /// 多行 data 已用 `\n` 拼好
    pub data: String,
}

/// 行驱动的 SSE 分帧器
#[derive(Default)]
pub struct SseParser {
    buf: String,
    data: Vec<String>,
    name: String,
}

impl SseParser {
    pub fn new() -> Self {
        SseParser {
            buf: String::new(),
            data: Vec::new(),
            name: String::new(),
        }
    }

    /// 喂一段文本，返回其中已经完整的事件
    pub fn push(&mut self, chunk: &str) -> Vec<Event> {
        let mut out = Vec::new();
        self.buf.push_str(chunk);
        // 只处理到最后一个换行为止，剩下的可能是半行
        while let Some(pos) = self.buf.find('\n') {
            let line: String = self.buf[..pos].to_string();
            self.buf.drain(..pos + 1);
            let line = line.strip_suffix('\r').unwrap_or(&line).to_string();
            if line.is_empty() {
                if let Some(ev) = self.dispatched() {
                    out.push(ev);
                }
                continue;
            }
            if line.starts_with(':') {
                // 注释行 / 心跳，直接丢
                continue;
            }
            if let Some(rest) = line.strip_prefix("data:") {
                let rest = rest.strip_prefix(' ').unwrap_or(rest);
                self.data.push(rest.to_string());
            } else if let Some(rest) = line.strip_prefix("event:") {
                self.name = rest.strip_prefix(' ').unwrap_or(rest).to_string();
            }
            // id: / retry: 忽略
        }
        out
    }

    /// 流结束时收尾：没有以空行结尾的最后一帧也要吐出来
    pub fn finish(&mut self) -> Vec<Event> {
        let mut out = Vec::new();
        if !self.buf.is_empty() {
            let tail = std::mem::take(&mut self.buf);
            out.extend(self.push(&(tail + "\n")));
        }
        if let Some(ev) = self.dispatched() {
            out.push(ev);
        }
        out
    }

    fn dispatched(&mut self) -> Option<Event> {
        if self.data.is_empty() {
            self.name.clear();
            return None;
        }
        let data = self.data.join("\n");
        self.data.clear();
        let name = std::mem::take(&mut self.name);
        Some(Event { name, data })
    }
}

/// `data: [DONE]` 判定（大小写不敏感、允许空白）
pub fn is_done(data: &str) -> bool {
    data.trim().eq_ignore_ascii_case("[done]")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn data_of(events: &[Event]) -> Vec<&str> {
        events.iter().map(|e| e.data.as_str()).collect()
    }

    #[test]
    fn single_event() {
        let mut p = SseParser::new();
        let ev = p.push("data: {\"a\":1}\n\n");
        assert_eq!(data_of(&ev), vec!["{\"a\":1}"]);
    }

    #[test]
    fn two_events_in_one_chunk() {
        let mut p = SseParser::new();
        let ev = p.push("data: one\n\ndata: two\n\n");
        assert_eq!(data_of(&ev), vec!["one", "two"]);
    }

    #[test]
    fn event_split_across_chunks() {
        let mut p = SseParser::new();
        assert!(p.push("data: {\"delta\":\"你").is_empty());
        let ev = p.push("好\"}\n\n");
        assert_eq!(data_of(&ev), vec!["{\"delta\":\"你好\"}"]);
    }

    #[test]
    fn crlf_framing() {
        let mut p = SseParser::new();
        let ev = p.push("data: hello\r\n\r\n");
        assert_eq!(data_of(&ev), vec!["hello"]);
    }

    #[test]
    fn multiline_data_joined() {
        let mut p = SseParser::new();
        let ev = p.push("data: line1\ndata: line2\n\n");
        assert_eq!(data_of(&ev), vec!["line1\nline2"]);
    }

    #[test]
    fn comments_and_heartbeats_ignored() {
        let mut p = SseParser::new();
        let ev = p.push(": ping\n\n: keep-alive\ndata: x\n\n");
        assert_eq!(data_of(&ev), vec!["x"]);
    }

    #[test]
    fn done_marker() {
        let mut p = SseParser::new();
        let ev = p.push("data: [DONE]\n\n");
        assert_eq!(data_of(&ev), vec!["[DONE]"]);
        assert!(is_done(&ev[0].data));
        assert!(is_done(" [done] "));
        assert!(!is_done("[DONE_EXTRA]"));
    }

    #[test]
    fn event_name_is_kept() {
        let mut p = SseParser::new();
        let ev = p.push("event: batch\ndata: {}\n\n");
        assert_eq!(ev.len(), 1);
        assert_eq!(ev[0].name, "batch");
        assert_eq!(ev[0].data, "{}");
    }

    #[test]
    fn finish_flushes_unterminated_tail() {
        let mut p = SseParser::new();
        assert!(p.push("data: tail").is_empty());
        let ev = p.finish();
        assert_eq!(data_of(&ev), vec!["tail"]);
    }

    #[test]
    fn utf8_split_across_byte_chunks() {
        let mut d = Utf8Decoder::new();
        let bytes = "你好".as_bytes();
        // 「你」的 3 个字节被切成 1 + 2
        assert_eq!(d.push(&bytes[..1]), "");
        assert_eq!(d.push(&bytes[1..4]), "你");
        assert_eq!(d.push(&bytes[4..]), "好");
        assert_eq!(d.finish(), "");
    }

    #[test]
    fn utf8_invalid_bytes_do_not_stall() {
        let mut d = Utf8Decoder::new();
        let s = d.push(&[b'a', 0xff, b'b']);
        assert!(s.starts_with('a'));
        assert!(s.ends_with('b'));
        assert!(s.contains('\u{fffd}'));
    }

    #[test]
    fn feed_byte_by_byte() {
        let mut d = Utf8Decoder::new();
        let mut p = SseParser::new();
        let payload = "data: {\"content\":\"中文测试\"}\n\n";
        let mut events = Vec::new();
        for b in payload.as_bytes() {
            let text = d.push(&[*b]);
            events.extend(p.push(&text));
        }
        events.extend(p.finish());
        assert_eq!(data_of(&events), vec!["{\"content\":\"中文测试\"}"]);
    }
}
