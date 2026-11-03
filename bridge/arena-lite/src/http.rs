//! 极简 HTTP/1.1 客户端（零依赖）。
//!
//! 桥接只监听本地明文 HTTP（127.0.0.1），所以这里只支持 http://，
//! 手写请求、解析响应头，并透明解开 chunked 传输编码，
//! 让上层可以按行读取 SSE 流。

use std::io::{self, BufReader, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct Endpoint {
    pub host: String,
    pub port: u16,
    /// base-url 里可能带路径前缀（如 http://127.0.0.1:17892/prefix）
    pub prefix: String,
}

pub fn parse_base_url(url: &str) -> Result<Endpoint, String> {
    let rest = url.strip_prefix("http://").ok_or_else(|| {
        if url.starts_with("https://") {
            "桥接只监听本地明文 HTTP（如 http://127.0.0.1:17892），不支持 https".to_string()
        } else {
            format!("base-url 需要以 http:// 开头，当前是: {}", url)
        }
    })?;
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, ""),
    };
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) => (
            h.to_string(),
            p.parse::<u16>()
                .map_err(|_| format!("base-url 端口不对: {}", url))?,
        ),
        None => (authority.to_string(), 80),
    };
    if host.is_empty() {
        return Err("base-url host 为空".into());
    }
    Ok(Endpoint {
        host,
        port,
        prefix: path.trim_end_matches('/').to_string(),
    })
}

pub struct Response {
    pub status: u16,
    pub reason: String,
    pub headers: Vec<(String, String)>,
    pub body: BodyReader,
}

impl Response {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

enum BodyKind {
    Chunked,
    Limited(u64),
    UntilEof,
}

/// 响应体读取器：对上层屏蔽 chunked 解码，支持按行读（SSE）和读全量。
pub struct BodyReader {
    r: BufReader<TcpStream>,
    kind: BodyKind,
    chunk_left: usize,
    eof: bool,
}

impl BodyReader {
    fn raw_line(&mut self) -> io::Result<Option<String>> {
        let mut buf = Vec::new();
        loop {
            let mut b = [0u8; 1];
            match self.r.read(&mut b) {
                Ok(0) => {
                    return Ok(if buf.is_empty() {
                        None
                    } else {
                        Some(String::from_utf8_lossy(&buf).into_owned())
                    })
                }
                Ok(_) => {
                    if b[0] == b'\n' {
                        if buf.last() == Some(&b'\r') {
                            buf.pop();
                        }
                        return Ok(Some(String::from_utf8_lossy(&buf).into_owned()));
                    }
                    buf.push(b[0]);
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// 取一个已解码的字节；流结束返回 None。
    fn fill(&mut self) -> io::Result<Option<u8>> {
        if self.eof {
            return Ok(None);
        }
        if matches!(self.kind, BodyKind::Chunked) {
            return self.fill_chunked();
        }
        let left = match &self.kind {
            BodyKind::Limited(n) => Some(*n),
            _ => None,
        };
        if left == Some(0) {
            self.eof = true;
            return Ok(None);
        }
        let mut b = [0u8; 1];
        match self.r.read(&mut b) {
            Ok(0) => {
                self.eof = true;
                Ok(None)
            }
            Ok(_) => {
                if let Some(n) = left {
                    self.kind = BodyKind::Limited(n - 1);
                }
                Ok(Some(b[0]))
            }
            Err(e) => Err(e),
        }
    }

    fn fill_chunked(&mut self) -> io::Result<Option<u8>> {
        loop {
                if self.chunk_left == 0 {
                    match self.raw_line()? {
                        None => {
                            self.eof = true;
                            return Ok(None);
                        }
                        Some(l) => {
                            let hex = l.split(';').next().unwrap_or("").trim();
                            let n = usize::from_str_radix(hex, 16).map_err(|e| {
                                io::Error::new(
                                    io::ErrorKind::InvalidData,
                                    format!("bad chunk size '{}': {}", hex, e),
                                )
                            })?;
                            if n == 0 {
                                // 0 块之后是 trailer，读到空行为止
                                loop {
                                    match self.raw_line()? {
                                        None => break,
                                        Some(t) if t.is_empty() => break,
                                        Some(_) => {}
                                    }
                                }
                                self.eof = true;
                                return Ok(None);
                            }
                            self.chunk_left = n;
                        }
                    }
                }
                let mut b = [0u8; 1];
                match self.r.read(&mut b) {
                    Ok(0) => {
                        self.eof = true;
                        return Ok(None);
                    }
                    Ok(_) => {
                        self.chunk_left -= 1;
                        if self.chunk_left == 0 {
                            // 块数据后跟 CRLF
                            let mut crlf = [0u8; 2];
                            let _ = self.r.read_exact(&mut crlf);
                        }
                        return Ok(Some(b[0]));
                    }
                    Err(e) => return Err(e),
                }
            }
    }

    /// 读一行（不含结尾 \n，已去掉 \r）；流结束且无数据返回 None。
    pub fn read_line(&mut self) -> io::Result<Option<String>> {
        let mut buf = Vec::new();
        loop {
            match self.fill()? {
                None => {
                    return Ok(if buf.is_empty() {
                        None
                    } else {
                        Some(String::from_utf8_lossy(&buf).into_owned())
                    })
                }
                Some(b) => {
                    if b == b'\n' {
                        if buf.last() == Some(&b'\r') {
                            buf.pop();
                        }
                        return Ok(Some(String::from_utf8_lossy(&buf).into_owned()));
                    }
                    buf.push(b);
                }
            }
        }
    }

    pub fn read_to_end(&mut self) -> io::Result<Vec<u8>> {
        let mut out = Vec::new();
        while let Some(b) = self.fill()? {
            out.push(b);
        }
        Ok(out)
    }
}

fn read_header_line(r: &mut BufReader<TcpStream>) -> io::Result<String> {
    let mut buf = Vec::new();
    loop {
        let mut b = [0u8; 1];
        match r.read(&mut b) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "eof while reading headers",
                ))
            }
            Ok(_) => {
                if b[0] == b'\n' {
                    if buf.last() == Some(&b'\r') {
                        buf.pop();
                    }
                    return Ok(String::from_utf8_lossy(&buf).into_owned());
                }
                buf.push(b[0]);
            }
            Err(e) => return Err(e),
        }
    }
}

/// 发一个请求并读完响应头。body 为 Some 时按 JSON POST 发送。
/// `timeout` 是读超时（SSE 长流靠桥接的 `: ping` 心跳续命，这里给足即可）。
pub fn request(
    ep: &Endpoint,
    method: &str,
    path: &str,
    headers: &[(String, String)],
    body: Option<&[u8]>,
    timeout: Duration,
) -> io::Result<Response> {
    let addr_str = format!("{}:{}", ep.host, ep.port);
    let addrs: Vec<_> = addr_str.as_str().to_socket_addrs()?.collect();
    let mut last_err: Option<io::Error> = None;
    let mut stream: Option<TcpStream> = None;
    for a in &addrs {
        match TcpStream::connect_timeout(a, Duration::from_secs(10)) {
            Ok(s) => {
                stream = Some(s);
                break;
            }
            Err(e) => last_err = Some(e),
        }
    }
    let mut stream = match stream {
        Some(s) => s,
        None => {
            return Err(last_err.unwrap_or_else(|| {
                io::Error::new(io::ErrorKind::Other, format!("cannot resolve/connect {}", addr_str))
            }))
        }
    };
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(Duration::from_secs(30)))?;
    let _ = stream.set_nodelay(true);

    let full_path = if ep.prefix.is_empty() {
        path.to_string()
    } else {
        format!("{}{}", ep.prefix, path)
    };
    let mut req = String::new();
    req.push_str(&format!("{} {} HTTP/1.1\r\n", method, full_path));
    req.push_str(&format!("Host: {}:{}\r\n", ep.host, ep.port));
    req.push_str("Connection: close\r\n");
    req.push_str("Accept: application/json, text/event-stream\r\n");
    for (k, v) in headers {
        req.push_str(&format!("{}: {}\r\n", k, v));
    }
    if let Some(b) = body {
        req.push_str("Content-Type: application/json\r\n");
        req.push_str(&format!("Content-Length: {}\r\n", b.len()));
    }
    req.push_str("\r\n");
    stream.write_all(req.as_bytes())?;
    if let Some(b) = body {
        stream.write_all(b)?;
    }
    stream.flush()?;

    let mut reader = BufReader::new(stream);
    let status_line = read_header_line(&mut reader)?;
    let mut parts = status_line.splitn(3, ' ');
    let _http = parts.next().unwrap_or("");
    let status: u16 = parts.next().unwrap_or("0").trim().parse().unwrap_or(0);
    let reason = parts.next().unwrap_or("").trim().to_string();
    let mut headers = Vec::new();
    loop {
        let l = read_header_line(&mut reader)?;
        if l.is_empty() {
            break;
        }
        if let Some((k, v)) = l.split_once(':') {
            headers.push((k.trim().to_string(), v.trim().to_string()));
        }
    }
    let chunked = headers.iter().any(|(k, v)| {
        k.eq_ignore_ascii_case("transfer-encoding") && v.to_ascii_lowercase().contains("chunked")
    });
    let cl = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.parse::<u64>().ok());
    let kind = if chunked {
        BodyKind::Chunked
    } else if let Some(n) = cl {
        BodyKind::Limited(n)
    } else {
        BodyKind::UntilEof
    };
    Ok(Response {
        status,
        reason,
        headers,
        body: BodyReader {
            r: reader,
            kind,
            chunk_left: 0,
            eof: false,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_urls() {
        let e = parse_base_url("http://127.0.0.1:17892").unwrap();
        assert_eq!((e.host.as_str(), e.port, e.prefix.as_str()), ("127.0.0.1", 17892, ""));
        let e = parse_base_url("http://localhost:9000/pfx/").unwrap();
        assert_eq!((e.host.as_str(), e.port, e.prefix.as_str()), ("localhost", 9000, "/pfx"));
        assert!(parse_base_url("https://x").is_err());
        assert!(parse_base_url("127.0.0.1").is_err());
    }
}
