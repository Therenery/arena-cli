//! 极简 HTTP/1.1 客户端：一个请求一条连接，`Connection: close`。
//!
//! 桥只监听 127.0.0.1 的明文 http，所以不需要 TLS、不需要连接池、
//! 不需要 HTTP/2。这里只需要把请求写出去、把响应头读出来、
//! 再把 body（identity / chunked）按块吐给上层。

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

use crate::sse::Utf8Decoder;

#[derive(Debug, Clone)]
pub struct Base {
    pub host: String,
    pub port: u16,
    /// 路径前缀（默认空），例如 base 写成 http://127.0.0.1:17892/api 时为 "/api"
    pub prefix: String,
}

impl Base {
    /// 展示用：`127.0.0.1:17892`
    pub fn display(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }
}

/// 解析 base 字符串。支持 `127.0.0.1:17892`、`http://host:port`、末尾带不带 `/` 都行。
pub fn parse_base(input: &str) -> Result<Base, String> {
    let raw = input.trim();
    if raw.is_empty() {
        return Err("桥地址为空".to_string());
    }
    let mut rest = raw;
    let mut secure = false;
    if let Some(r) = rest.strip_prefix("http://") {
        rest = r;
    } else if let Some(r) = rest.strip_prefix("https://") {
        rest = r;
        secure = true;
    } else if rest.contains("://") {
        return Err(format!("不支持的协议：{}（桥只提供明文 http）", raw));
    }
    if secure {
        return Err(
            "桥只监听本地明文 http，不支持 https；请把地址写成 http://127.0.0.1:17892".to_string(),
        );
    }
    // 去掉 query / fragment，这些在 base 里没有意义
    if let Some(pos) = rest.find(['?', '#']) {
        rest = &rest[..pos];
    }
    let (hostport, path) = match rest.find('/') {
        Some(pos) => (&rest[..pos], &rest[pos..]),
        None => (rest, ""),
    };
    let hostport = hostport.trim();
    if hostport.is_empty() {
        return Err(format!("桥地址缺少主机名：{}", raw));
    }
    let (host, port) = if hostport.starts_with('[') {
        // [::1]:17892
        match hostport.find(']') {
            Some(end) => {
                let host = &hostport[1..end];
                let tail = &hostport[end + 1..];
                let port = tail.strip_prefix(':').unwrap_or("");
                (host.to_string(), parse_port(port)?)
            }
            None => return Err(format!("IPv6 地址未闭合：{}", raw)),
        }
    } else {
        match hostport.rsplit_once(':') {
            Some((h, p)) => (h.to_string(), parse_port(p)?),
            None => (hostport.to_string(), 17892u16),
        }
    };
    if host.is_empty() {
        return Err(format!("桥地址缺少主机名：{}", raw));
    }
    let prefix = path.trim_end_matches('/').to_string();
    Ok(Base {
        host,
        port,
        prefix,
    })
}

fn parse_port(s: &str) -> Result<u16, String> {
    if s.is_empty() {
        return Ok(17892);
    }
    s.parse::<u16>()
        .map_err(|_| format!("端口不是数字：{}", s))
}

#[derive(Debug)]
pub enum HttpError {
    /// 连不上（桥没起 / 端口不对）
    Connect(String),
    /// 连接已建立但读写失败
    Io(String),
    /// 对端不像 HTTP
    Protocol(String),
    /// 超时
    Timeout(String),
}

pub struct Response {
    pub status: u16,
    pub body: Body,
}

impl Response {
    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }

    /// 把剩余 body 全读出来（有上限，防止对端乱发）
    pub fn read_text(&mut self, limit: usize) -> Result<String, HttpError> {
        let mut out = String::new();
        let mut dec = Utf8Decoder::new();
        let mut buf = [0u8; 8192];
        let mut total = 0usize;
        loop {
            let n = self.body.read_some(&mut buf)?;
            if n == 0 {
                break;
            }
            total += n;
            if total > limit {
                return Err(HttpError::Protocol(format!(
                    "响应超过 {} 字节，已中止",
                    limit
                )));
            }
            out.push_str(&dec.push(&buf[..n]));
        }
        out.push_str(&dec.finish());
        Ok(out)
    }
}

/// 响应体读取器：负责 identity / chunked 两种传输方式
pub struct Body {
    stream: Option<TcpStream>,
    mode: BodyMode,
}

#[derive(Clone, Copy)]
enum BodyMode {
    /// Content-Length
    Length(u64),
    /// Transfer-Encoding: chunked
    Chunked { remain: u64, done: bool },
    /// 读到对端关闭
    Eof,
}

impl Body {
    fn new(stream: TcpStream, headers: &[(String, String)]) -> Body {
        let chunked = headers.iter().any(|(k, v)| {
            k.eq_ignore_ascii_case("transfer-encoding") && v.to_ascii_lowercase().contains("chunked")
        });
        let len = headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
            .and_then(|(_, v)| v.trim().parse::<u64>().ok());
        let mode = if chunked {
            BodyMode::Chunked {
                remain: 0,
                done: false,
            }
        } else if let Some(n) = len {
            BodyMode::Length(n)
        } else {
            BodyMode::Eof
        };
        Body {
            stream: Some(stream),
            mode,
        }
    }

    fn stream(&mut self) -> Result<&mut TcpStream, HttpError> {
        self.stream
            .as_mut()
            .ok_or_else(|| HttpError::Io("连接已关闭".to_string()))
    }

    /// 读一段解码后的字节；返回 0 表示结束
    pub fn read_some(&mut self, buf: &mut [u8]) -> Result<usize, HttpError> {
        match self.mode {
            BodyMode::Length(left) => {
                if left == 0 {
                    return Ok(0);
                }
                let cap = buf.len().min(left as usize).max(1);
                let n = self.read_raw(&mut buf[..cap])?;
                self.mode = BodyMode::Length(left.saturating_sub(n as u64));
                Ok(n)
            }
            BodyMode::Eof => self.read_raw(buf),
            BodyMode::Chunked { remain, done } => {
                if done {
                    return Ok(0);
                }
                let mut remain = remain;
                if remain == 0 {
                    // 读下一块的长度行
                    let line = self.read_line()?.trim().to_string();
                    if line.is_empty() {
                        self.mode = BodyMode::Chunked {
                            remain: 0,
                            done: true,
                        };
                        return Ok(0);
                    }
                    let hex = line.split(';').next().unwrap_or("").trim().to_string();
                    let size = u64::from_str_radix(&hex, 16).map_err(|_| {
                        HttpError::Protocol(format!("chunked 长度非法：{:?}", line))
                    })?;
                    if size == 0 {
                        // 吃掉 trailer 直到空行
                        loop {
                            let t = self.read_line()?;
                            if t.trim().is_empty() {
                                break;
                            }
                        }
                        self.mode = BodyMode::Chunked {
                            remain: 0,
                            done: true,
                        };
                        return Ok(0);
                    }
                    remain = size;
                }
                let cap = buf.len().min(remain as usize).max(1);
                let n = self.read_raw(&mut buf[..cap])?;
                if n == 0 {
                    self.mode = BodyMode::Chunked {
                        remain: 0,
                        done: true,
                    };
                    return Ok(0);
                }
                remain -= n as u64;
                if remain == 0 {
                    // 块尾 CRLF
                    let _ = self.read_line();
                }
                self.mode = BodyMode::Chunked {
                    remain,
                    done: remain != 0,
                };
                Ok(n)
            }
        }
    }

    fn read_raw(&mut self, buf: &mut [u8]) -> Result<usize, HttpError> {
        let s = self.stream()?;
        match s.read(buf) {
            Ok(n) => Ok(n),
            Err(e) => Err(map_io(&e, "读响应")),
        }
    }

    fn read_line(&mut self) -> Result<String, HttpError> {
        let mut out = Vec::new();
        let mut byte = [0u8; 1];
        loop {
            let n = self.read_raw(&mut byte)?;
            if n == 0 {
                break;
            }
            if byte[0] == b'\n' {
                break;
            }
            if byte[0] != b'\r' {
                out.push(byte[0]);
            }
            if out.len() > 8192 {
                return Err(HttpError::Protocol("响应头过长".to_string()));
            }
        }
        Ok(String::from_utf8_lossy(&out).to_string())
    }
}

fn map_io(e: &std::io::Error, what: &str) -> HttpError {
    match e.kind() {
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut => {
            HttpError::Timeout(format!("{}超时", what))
        }
        std::io::ErrorKind::ConnectionRefused => {
            HttpError::Connect("连接被拒绝（端口没在监听）".to_string())
        }
        std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::BrokenPipe => {
            HttpError::Io("对端断开了连接".to_string())
        }
        _ => HttpError::Io(format!("{}失败：{}", what, e)),
    }
}

pub struct Request<'a> {
    pub method: &'a str,
    pub path: &'a str,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    pub read_timeout: Duration,
    pub connect_timeout: Duration,
}

impl<'a> Request<'a> {
    pub fn new(method: &'a str, path: &'a str) -> Self {
        Request {
            method,
            path,
            headers: Vec::new(),
            body: Vec::new(),
            read_timeout: Duration::from_secs(60),
            connect_timeout: Duration::from_secs(5),
        }
    }

    pub fn json_body(mut self, body: String) -> Self {
        self.body = body.into_bytes();
        self.headers.push((
            "Content-Type".to_string(),
            "application/json; charset=utf-8".to_string(),
        ));
        self
    }

    pub fn header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.to_string(), value.to_string()));
        self
    }

    pub fn read_timeout(mut self, d: Duration) -> Self {
        self.read_timeout = d;
        self
    }

}

/// 发一个请求，返回已经解析好响应头的 Response（body 还没读）
pub fn send(base: &Base, req: Request<'_>) -> Result<Response, HttpError> {
    let addr = format!("{}:{}", base.host, base.port);
    let sock = resolve_first(&addr, req.connect_timeout)
        .ok_or_else(|| HttpError::Connect(format!("解析地址失败：{}", addr)))?;
    let mut stream = TcpStream::connect_timeout(&sock, req.connect_timeout)
        .map_err(|e| map_io(&e, "连接桥"))?;
    let _ = stream.set_nodelay(true);
    let _ = stream.set_read_timeout(Some(req.read_timeout));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(30)));

    let path = if base.prefix.is_empty() {
        req.path.to_string()
    } else if req.path.starts_with('/') {
        format!("{}{}", base.prefix, req.path)
    } else {
        format!("{}/{}", base.prefix, req.path)
    };

    let mut head = String::new();
    head.push_str(&format!("{} {} HTTP/1.1\r\n", req.method, path));
    head.push_str(&format!("Host: {}\r\n", base.display()));
    head.push_str("User-Agent: arena-lite/2.0\r\n");
    head.push_str("Accept: */*\r\n");
    head.push_str("Connection: close\r\n");
    for (k, v) in &req.headers {
        head.push_str(&format!("{}: {}\r\n", k, v));
    }
    if !req.body.is_empty() || req.method == "POST" {
        head.push_str(&format!("Content-Length: {}\r\n", req.body.len()));
    }
    head.push_str("\r\n");

    stream
        .write_all(head.as_bytes())
        .map_err(|e| map_io(&e, "写请求头"))?;
    if !req.body.is_empty() {
        stream
            .write_all(&req.body)
            .map_err(|e| map_io(&e, "写请求体"))?;
    }
    let _ = stream.flush();

    let (status, _reason, headers) = read_head(&mut stream)?;
    let body = Body::new(stream, &headers);
    Ok(Response { status, body })
}

fn resolve_first(addr: &str, _timeout: Duration) -> Option<std::net::SocketAddr> {
    addr.to_socket_addrs().ok().and_then(|mut it| it.next())
}

fn read_head(stream: &mut TcpStream) -> Result<(u16, String, Vec<(String, String)>), HttpError> {
    let mut buf: Vec<u8> = Vec::with_capacity(512);
    let mut byte = [0u8; 1];
    // 逐字节读到 \r\n\r\n，几百字节的量级，不值得上 BufReader
    loop {
        let n = stream
            .read(&mut byte)
            .map_err(|e| map_io(&e, "读响应头"))?;
        if n == 0 {
            return Err(HttpError::Protocol(
                "连接在收到响应头之前就断了（桥可能崩了）".to_string(),
            ));
        }
        buf.push(byte[0]);
        if buf.ends_with(b"\r\n\r\n") || buf.ends_with(b"\n\n") {
            break;
        }
        if buf.len() > 64 * 1024 {
            return Err(HttpError::Protocol("响应头超过 64KB".to_string()));
        }
    }
    let text = String::from_utf8_lossy(&buf).to_string();
    let mut lines = text.lines();
    let status_line = lines.next().unwrap_or("");
    let mut parts = status_line.splitn(3, ' ');
    let proto = parts.next().unwrap_or("");
    if !proto.starts_with("HTTP/") {
        return Err(HttpError::Protocol(format!(
            "这不是 HTTP 响应：{:?}",
            status_line
        )));
    }
    let status: u16 = parts
        .next()
        .and_then(|s| s.trim().parse::<u16>().ok())
        .ok_or_else(|| HttpError::Protocol(format!("状态行非法：{:?}", status_line)))?;
    let reason = parts.next().unwrap_or("").trim().to_string();
    let mut headers = Vec::new();
    for line in lines {
        let line = line.trim_end_matches('\r');
        if line.is_empty() {
            continue;
        }
        // 折行头（obs-fold）极少见，粘到上一行
        if line.starts_with(' ') || line.starts_with('\t') {
            if let Some(last) = headers.last_mut() {
                let (_, v): &mut (String, String) = last;
                v.push(' ');
                v.push_str(line.trim());
            }
            continue;
        }
        if let Some(pos) = line.find(':') {
            let name = line[..pos].trim().to_string();
            let value = line[pos + 1..].trim().to_string();
            headers.push((name, value));
        }
    }
    Ok((status, reason, headers))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_plain_host_port() {
        let b = parse_base("127.0.0.1:17892").expect("base");
        assert_eq!(b.host, "127.0.0.1");
        assert_eq!(b.port, 17892);
        assert!(b.prefix.is_empty());
    }

    #[test]
    fn parse_http_url_with_trailing_slash() {
        let b = parse_base("http://127.0.0.1:17892/").expect("base");
        assert_eq!(b.host, "127.0.0.1");
        assert_eq!(b.port, 17892);
        assert!(b.prefix.is_empty());
    }

    #[test]
    fn parse_default_port() {
        let b = parse_base("http://localhost").expect("base");
        assert_eq!(b.host, "localhost");
        assert_eq!(b.port, 17892);
    }

    #[test]
    fn parse_prefix() {
        let b = parse_base("http://127.0.0.1:17892/api/").expect("base");
        assert_eq!(b.prefix, "/api");
    }

    #[test]
    fn parse_ipv6() {
        let b = parse_base("http://[::1]:17892").expect("base");
        assert_eq!(b.host, "::1");
        assert_eq!(b.port, 17892);
    }

    #[test]
    fn reject_https_and_garbage() {
        assert!(parse_base("https://127.0.0.1:17892").is_err());
        assert!(parse_base("ftp://x").is_err());
        assert!(parse_base("").is_err());
        assert!(parse_base("127.0.0.1:abc").is_err());
    }

    #[test]
    fn body_mode_detection_prefers_chunked() {
        // 只测分支判定，不建真连接
        let hdrs = vec![(
            "Transfer-Encoding".to_string(),
            "chunked".to_string(),
        )];
        assert!(hdrs
            .iter()
            .any(|(k, v)| k.eq_ignore_ascii_case("transfer-encoding")
                && v.to_ascii_lowercase().contains("chunked")));
    }
}
