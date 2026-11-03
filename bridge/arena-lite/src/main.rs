//! arena-lite —— arena_bridge 的轻量 Rust 客户端。
//!
//! 只做一件事：以最快的路径把终端连到本地桥接服务（默认 http://127.0.0.1:17892），
//! 完整覆盖桥接的对话与运维接口，不掺别的功能。

mod bridge;
mod http;
mod json;
mod tui;

use bridge::{fmt_health, Bridge, ChatRequest, StreamEvent, DEFAULT_BASE};
use std::io::{self, Write};
use std::process::exit;
use std::sync::mpsc::channel;
use std::thread;

pub struct Config {
    pub base_url: String,
    pub token: Option<String>,
    pub session: String,
    pub model: String,
    pub mode: String,
    pub reasoning: bool,
    pub fresh: bool,
    pub no_stream: bool,
    pub timeout: u64,
}

enum Cmd {
    Tui,
    Ask(String),
    Models,
    Health,
    New(Option<String>),
    Debug,
    Repos,
    Repo(String),
    Upload(Vec<String>),
    Pr { n: Option<String>, repo: Option<String> },
    Battle { prompt: String, models: Vec<String> },
    Vote(String),
    Help,
    Version,
}

struct Opts {
    base_url: Option<String>,
    token: Option<String>,
    session: Option<String>,
    model: Option<String>,
    mode: Option<String>,
    reasoning: bool,
    fresh: bool,
    no_stream: bool,
    timeout: Option<u64>,
    repo: Option<String>,
    models: Option<String>,
    direct: bool,
    cmd: Option<String>,
    positional: Vec<String>,
}

fn env_any(keys: &[&str]) -> Option<String> {
    for k in keys {
        if let Ok(v) = std::env::var(k) {
            let v = v.trim().to_string();
            if !v.is_empty() {
                return Some(v);
            }
        }
    }
    None
}

const COMMANDS: &[&str] = &[
    "chat", "tui", "ask", "models", "health", "doctor", "new", "debug", "repos", "repo", "upload",
    "pr", "battle", "vote", "help", "version",
];

fn usage() -> String {
    format!(
        r#"arena-lite {} —— arena_bridge（arena.ai Agent 桥接）的轻量 Rust 客户端

用法:
  arena-lite                      进入对话 TUI（流式，默认 arena-agent / 会话 main）
  arena-lite "问题"               单发提问，流式打印后退出
  arena-lite ask "问题"           同上

对话选项:
  -d, --direct            direct 模式（模型 arena-direct，单模型更快）
  -a, --agent             agent 模式（默认，可跑工具、读仓库）
  -m, --model <id>        指定模型 id
      --mode <模式>       覆盖页面模式（agent|direct），默认随模型走
  -s, --session <名>      会话名（默认 main，或 $ARENA_SESSION）
  -r, --reasoning         显示思考过程（TUI 里用 /reasoning 切换）
  -n, --new               本轮前清空服务端会话历史
      --no-stream         单发时等全部完成再打印（默认流式实时出字）

连接:
      --base-url <url>    桥接地址（默认 {}，或 $ARENA_BRIDGE_URL / $ARB_BRIDGE_URL）
      --token <t>         鉴权 token（或 $ARENA_TOKEN / $ARB_TOKEN，对应桥接 ARB_TOKEN）
      --timeout <秒>      单次请求超时（默认 900，agent 任务要 40s~5min）

运维子命令:
  models                  列出模型（/v1/models + 路由模型）
  health | doctor         桥接体检（chrome / 队列 / 会话）
  debug                   会话内部状态
  new [会话]              清空会话（服务端历史 + 标签页）
  repos                   列出工作区可切换的 GitHub 仓库
  repo <owner/name>       切换工作区仓库（等于开新对话）
  upload <文件...>        把本地文件挂到当前会话输入框
  pr [编号] [--repo o/n]  列 PR 或查看单个 PR（默认当前工作区仓库）
  battle "问题" [--models a,b]   双模型对战
  vote <a|b|tie|both>     给上一轮对战投票
  help / version

环境变量: ARENA_BRIDGE_URL, ARENA_TOKEN(或 ARB_TOKEN), ARENA_SESSION, ARENA_TIMEOUT
TUI 内: /help 查看全部命令；Enter 发送，Ctrl+C 退出，↑↓ 历史，PgUp/PgDn 滚动"#,
        env!("CARGO_PKG_VERSION"),
        DEFAULT_BASE
    )
}

fn parse_args(args: &[String]) -> Result<(Cmd, Opts), String> {
    macro_rules! need_val {
        () => {
            match val {
                Some(v) => v.to_string(),
                None => {
                    i += 1;
                    if i >= args.len() {
                        return Err(format!("选项 {} 缺少参数", name));
                    }
                    args[i].clone()
                }
            }
        };
    }
    let mut o = Opts {
        base_url: None,
        token: None,
        session: None,
        model: None,
        mode: None,
        reasoning: false,
        fresh: false,
        no_stream: false,
        timeout: None,
        repo: None,
        models: None,
        direct: false,
        cmd: None,
        positional: Vec::new(),
    };
    let mut i = 0;
    let mut only_positional = false;
    while i < args.len() {
        let a = args[i].as_str();
        if !only_positional && a == "--" {
            only_positional = true;
            i += 1;
            continue;
        }
        if !only_positional && a.starts_with('-') && a.len() > 1 {
            let mut val: Option<&str> = None;
            let name = match a.split_once('=') {
                Some((n, v)) => {
                    val = Some(v);
                    n
                }
                None => a,
            };
            match name {
                "-d" | "--direct" => o.direct = true,
                "-a" | "--agent" => o.direct = false,
                "-r" | "--reasoning" => o.reasoning = true,
                "-n" | "--new" => o.fresh = true,
                "--no-stream" => o.no_stream = true,
                "-s" | "--session" => o.session = Some(need_val!()),
                "-m" | "--model" => o.model = Some(need_val!()),
                "--mode" => o.mode = Some(need_val!()),
                "--base-url" => o.base_url = Some(need_val!()),
                "--token" => o.token = Some(need_val!()),
                "--timeout" => {
                    o.timeout = Some(
                        need_val!()
                            .parse::<u64>()
                            .map_err(|_| "--timeout 需要正整数（秒）".to_string())?,
                    )
                }
                "--repo" => o.repo = Some(need_val!()),
                "--models" => o.models = Some(need_val!()),
                "-h" | "--help" => o.cmd = Some("help".to_string()),
                "-V" | "--version" => o.cmd = Some("version".to_string()),
                _ => return Err(format!("未知选项: {}（试试 arena-lite help）", a)),
            }
        } else if o.cmd.is_none() && COMMANDS.contains(&a) {
            o.cmd = Some(a.to_string());
        } else {
            o.positional.push(a.to_string());
        }
        i += 1;
    }

    let cmd = match o.cmd.as_deref() {
        Some("chat") | Some("tui") => Cmd::Tui,
        Some("ask") => {
            let p = o.positional.join(" ");
            if p.trim().is_empty() {
                return Err("ask 需要一个问题，例如: arena-lite ask \"你好\"".into());
            }
            Cmd::Ask(p)
        }
        Some("models") => Cmd::Models,
        Some("health") | Some("doctor") => Cmd::Health,
        Some("new") => Cmd::New(o.positional.first().cloned()),
        Some("debug") => Cmd::Debug,
        Some("repos") => Cmd::Repos,
        Some("repo") => {
            let name = o.positional.join(" ");
            if name.trim().is_empty() {
                return Err("用法: arena-lite repo <owner/name>".into());
            }
            Cmd::Repo(name.trim().to_string())
        }
        Some("upload") => {
            if o.positional.is_empty() {
                return Err("用法: arena-lite upload <文件路径...>".into());
            }
            Cmd::Upload(o.positional.clone())
        }
        Some("pr") => Cmd::Pr {
            n: o.positional.first().cloned(),
            repo: o.repo.clone(),
        },
        Some("battle") => {
            let p = o.positional.join(" ");
            if p.trim().is_empty() {
                return Err("用法: arena-lite battle \"问题\" [--models a,b]".into());
            }
            let models = o
                .models
                .clone()
                .map(|m| m.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect())
                .unwrap_or_default();
            Cmd::Battle {
                prompt: p,
                models,
            }
        }
        Some("vote") => {
            let c = o.positional.first().cloned().unwrap_or_default();
            if !["a", "b", "tie", "both"].contains(&c.as_str()) {
                return Err("用法: arena-lite vote <a|b|tie|both>".into());
            }
            Cmd::Vote(c)
        }
        Some("help") => Cmd::Help,
        Some("version") => Cmd::Version,
        _ => {
            if o.positional.is_empty() {
                Cmd::Tui
            } else {
                Cmd::Ask(o.positional.join(" "))
            }
        }
    };
    Ok((cmd, o))
}

fn resolve_config(o: &Opts) -> Config {
    let timeout = o
        .timeout
        .or_else(|| env_any(&["ARENA_TIMEOUT"]).and_then(|v| v.parse().ok()))
        .unwrap_or(900);
    Config {
        base_url: o
            .base_url
            .clone()
            .or_else(|| env_any(&["ARENA_BRIDGE_URL", "ARB_BRIDGE_URL"]))
            .unwrap_or_else(|| DEFAULT_BASE.to_string()),
        token: o
            .token
            .clone()
            .or_else(|| env_any(&["ARENA_TOKEN", "ARB_TOKEN"])),
        session: o
            .session
            .clone()
            .or_else(|| env_any(&["ARENA_SESSION"]))
            .unwrap_or_else(|| "main".to_string()),
        model: o
            .model
            .clone()
            .unwrap_or_else(|| if o.direct { "arena-direct" } else { "arena-agent" }.to_string()),
        mode: o
            .mode
            .clone()
            .unwrap_or_else(|| if o.direct { "direct" } else { "agent" }.to_string()),
        reasoning: o.reasoning || env_any(&["ARENA_REASONING"]).map(|v| v == "1" || v == "true").unwrap_or(false),
        fresh: o.fresh,
        no_stream: o.no_stream,
        timeout,
    }
}

fn mk_bridge(cfg: &Config) -> Bridge {
    match Bridge::new(&cfg.base_url, cfg.token.clone(), cfg.timeout) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("✗ {}", e);
            exit(2);
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (cmd, opts) = match parse_args(&args) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("✗ {}", e);
            eprintln!("试试 `arena-lite help`");
            exit(2);
        }
    };
    let cfg = resolve_config(&opts);
    let code = dispatch(cmd, &cfg);
    exit(code);
}

fn dispatch(cmd: Cmd, cfg: &Config) -> i32 {
    match cmd {
        Cmd::Help => {
            print!("{}", usage());
            0
        }
        Cmd::Version => {
            println!("arena-lite {}", env!("CARGO_PKG_VERSION"));
            0
        }
        Cmd::Tui => tui::run(cfg),
        Cmd::Ask(p) => run_ask(cfg, &p),
        Cmd::Models => cmd_models(cfg),
        Cmd::Health => cmd_health(cfg),
        Cmd::New(s) => {
            let b = mk_bridge(cfg);
            let name = s.unwrap_or_else(|| cfg.session.clone());
            match b.reset(&name) {
                Ok(_) => {
                    println!("已清空会话 {}", name);
                    0
                }
                Err(e) => {
                    eprintln!("✗ {}", e);
                    1
                }
            }
        }
        Cmd::Debug => {
            let b = mk_bridge(cfg);
            match b.debug(&cfg.session) {
                Ok(j) => {
                    println!("{}", j.pretty());
                    0
                }
                Err(e) => {
                    eprintln!("✗ {}", e);
                    1
                }
            }
        }
        Cmd::Repos => {
            let b = mk_bridge(cfg);
            match b.repos(&cfg.session, false) {
                Ok(j) => {
                    println!("当前仓库: {}", j.get("repo").and_then(|v| v.as_str()).unwrap_or("?"));
                    if let Some(arr) = j.get("repos").and_then(|v| v.as_arr()) {
                        for r in arr {
                            println!("  {}", r.as_str().unwrap_or("?"));
                        }
                    }
                    0
                }
                Err(e) => {
                    eprintln!("✗ {}", e);
                    1
                }
            }
        }
        Cmd::Repo(name) => {
            let b = mk_bridge(cfg);
            match b.switch_repo(&cfg.session, &name) {
                Ok(j) => {
                    println!(
                        "已切到 {}，新对话已开",
                        j.get("repo").and_then(|v| v.as_str()).unwrap_or(&name)
                    );
                    0
                }
                Err(e) => {
                    eprintln!("✗ {}", e);
                    1
                }
            }
        }
        Cmd::Upload(files) => {
            let b = mk_bridge(cfg);
            match b.upload(&cfg.session, &files) {
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
                    println!("已挂载到会话 {}: {}", cfg.session, att);
                    0
                }
                Err(e) => {
                    eprintln!("✗ {}", e);
                    1
                }
            }
        }
        Cmd::Pr { n, repo } => {
            let b = mk_bridge(cfg);
            let repo = match repo {
                Some(r) => r,
                None => match b.repos(&cfg.session, true) {
                    Ok(j) => j
                        .get("repo")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| {
                            eprintln!("✗ 拿不到当前仓库，请用 --repo <owner/name> 指定");
                            exit(2);
                        }),
                    Err(e) => {
                        eprintln!("✗ {}", e);
                        exit(1);
                    }
                },
            };
            match b.pr(&repo, n.as_deref()) {
                Ok(j) => {
                    if let Some(prs) = j.get("prs").and_then(|v| v.as_arr()) {
                        for p in prs {
                            let num = p.get("number").and_then(|v| v.as_f64()).unwrap_or(0.0) as i64;
                            let title = p.get("title").and_then(|v| v.as_str()).unwrap_or("");
                            let state = p.get("state").and_then(|v| v.as_str()).unwrap_or("");
                            let url = p.get("url").and_then(|v| v.as_str()).unwrap_or("");
                            println!("#{} [{}] {}  {}", num, state, title, url);
                        }
                    } else if let Some(p) = j.get("pr") {
                        println!("{}", p.pretty());
                    } else {
                        println!("{}", j.pretty());
                    }
                    0
                }
                Err(e) => {
                    eprintln!("✗ {}", e);
                    1
                }
            }
        }
        Cmd::Battle { prompt, models } => {
            let b = mk_bridge(cfg);
            eprintln!("对战中（可能要 1~5 分钟）...");
            match b.battle(&cfg.session, &prompt, "battle", &models) {
                Ok(j) => {
                    let pane = |k: &str| {
                        j.get(k)
                            .map(|p| {
                                let label = p.get("label").and_then(|v| v.as_str()).unwrap_or(k);
                                let text = p.get("text").and_then(|v| v.as_str()).unwrap_or("");
                                format!("== {} · {} ==\n\n{}", k.to_uppercase(), label, text)
                            })
                            .unwrap_or_default()
                    };
                    println!("{}\n\n---\n\n{}\n\n投票: arena-lite vote <a|b|tie|both>", pane("a"), pane("b"));
                    0
                }
                Err(e) => {
                    eprintln!("✗ {}", e);
                    1
                }
            }
        }
        Cmd::Vote(c) => {
            let b = mk_bridge(cfg);
            match b.vote(&cfg.session, &c) {
                Ok(j) => {
                    println!(
                        "已投票: {}（胜方: {}）",
                        j.get("choice").and_then(|v| v.as_str()).unwrap_or(&c),
                        j.get("winner").and_then(|v| v.as_str()).unwrap_or("?")
                    );
                    0
                }
                Err(e) => {
                    eprintln!("✗ {}", e);
                    1
                }
            }
        }
    }
}

fn cmd_models(cfg: &Config) -> i32 {
    let b = mk_bridge(cfg);
    match b.models_v1() {
        Ok(ms) => {
            println!("聊天模型:");
            for m in ms {
                println!("  {}", m);
            }
        }
        Err(e) => {
            eprintln!("✗ {}", e);
            return 1;
        }
    }
    if let Ok(ms) = b.models_routing() {
        if !ms.is_empty() {
            println!("路由模型: {}", ms.join(", "));
        }
    }
    0
}

fn cmd_health(cfg: &Config) -> i32 {
    let b = mk_bridge(cfg);
    match b.health() {
        Ok(j) => {
            println!("{}", fmt_health(&j));
            0
        }
        Err(e) => {
            eprintln!("✗ 桥接不可达: {}", e);
            1
        }
    }
}

/// 单发提问：默认流式实时出字到 stdout；思考过程（-r）走 stderr 变暗。
fn run_ask(cfg: &Config, prompt: &str) -> i32 {
    let b = mk_bridge(cfg);
    let req = ChatRequest {
        model: cfg.model.clone(),
        session: cfg.session.clone(),
        mode: cfg.mode.clone(),
        fresh: cfg.fresh,
        messages: vec![("user".to_string(), prompt.to_string())],
    };
    if cfg.no_stream {
        return match b.chat_once(&req) {
            Ok(r) => {
                if cfg.reasoning && !r.reasoning.is_empty() {
                    eprintln!("\x1b[2m{}\x1b[0m", r.reasoning);
                }
                if let Some(rt) = &r.routed {
                    eprintln!("\x1b[2m· routed -> {}\x1b[0m", rt);
                }
                print!("{}", r.text);
                if !r.text.ends_with('\n') {
                    println!();
                }
                if r.truncated {
                    eprintln!("\x1b[2m· 问题超长，已被桥接截断\x1b[0m");
                }
                0
            }
            Err(e) => {
                eprintln!("✗ {}", e);
                1
            }
        };
    }
    let (tx, rx) = channel::<StreamEvent>();
    let b2 = b.clone();
    let th = thread::spawn(move || {
        let r = b2.chat_stream(&req, |e| {
            let _ = tx.send(e);
        });
        if let Err(e) = r {
            let _ = tx.send(StreamEvent::Error(e));
        }
    });
    let stdout = io::stdout();
    let mut out = stdout.lock();
    let mut err = io::stderr();
    let mut code = 0;
    loop {
        match rx.recv() {
            Ok(StreamEvent::Text(t)) => {
                let _ = out.write_all(t.as_bytes());
                let _ = out.flush();
            }
            Ok(StreamEvent::Reasoning(t)) => {
                if cfg.reasoning {
                    let _ = err.write_all(format!("\x1b[2m{}\x1b[0m", t).as_bytes());
                    let _ = err.flush();
                }
            }
            Ok(StreamEvent::Error(e)) => {
                let _ = writeln!(err, "\n✗ {}", e);
                code = 1;
            }
            Ok(StreamEvent::Done) => break,
            Err(_) => break,
        }
    }
    let _ = th.join();
    if code == 0 {
        let _ = writeln!(out);
    }
    code
}
