//! arena-lite —— arena_bridge 的轻量终端客户端（零第三方依赖）。
//!
//! 只做桥接这一件事：常驻模式对话、对战、历史载入、体检。
//! 没有渠道、没有沙箱、没有扩展、没有子代理。

mod app;
mod client;
mod err;
mod event;
mod foreground;
mod httpc;
mod json;
mod keys;
mod mode;
mod sse;
mod term;
mod theme;
mod ui;
mod urlenc;
mod width;

use std::io::{IsTerminal, Write};
use std::sync::mpsc::{channel, Receiver};
use std::sync::Arc;
use std::time::{Duration, Instant};

use app::{App, Config};
use client::{ChatOpts, Client, Msg as ChatMsg, StreamEvent};
use err::BridgeError;
use event::Msg;
use httpc::Base;
use mode::Mode;
use theme::{Palette, Theme};

const VERSION: &str = "2.0.0";
const DEFAULT_BASE: &str = "127.0.0.1:17892";

/* ------------------------------ 参数 ------------------------------ */

#[derive(Debug, PartialEq)]
enum Cmd {
    Tui { prompt: Option<String> },
    OneShot { prompt: String },
    Health,
    Models,
    History { session: String },
    Resume { session: String, prompt: Option<String> },
    Version,
    Help,
    BadUsage(String),
}

struct Opts {
    base: String,
    token: Option<String>,
    session: String,
    mode: Mode,
    models: Vec<String>,
    logo: bool,
    reasoning: bool,
    color: bool,
    cmd: Cmd,
}

fn env_flag(name: &str) -> bool {
    match std::env::var(name) {
        Ok(v) => {
            let v = v.trim().to_ascii_lowercase();
            !(v.is_empty() || v == "0" || v == "false" || v == "no" || v == "off")
        }
        Err(_) => false,
    }
}

fn parse_args(args: Vec<String>) -> Opts {
    let mut opts = Opts {
        base: std::env::var("ARENA_BASE").unwrap_or_else(|_| DEFAULT_BASE.to_string()),
        token: std::env::var("ARENA_TOKEN")
            .ok()
            .or_else(|| std::env::var("ARB_TOKEN").ok())
            .filter(|t| !t.trim().is_empty()),
        session: std::env::var("ARENA_SESSION").unwrap_or_else(|_| "main".to_string()),
        mode: std::env::var("ARENA_MODE")
            .ok()
            .and_then(|m| Mode::parse(&m))
            .unwrap_or(Mode::Agent),
        models: Vec::new(),
        logo: !env_flag("ARENA_NO_LOGO"),
        reasoning: env_flag("ARENA_REASONING"),
        color: !env_flag("NO_COLOR") && !env_flag("ARENA_NO_COLOR"),
        cmd: Cmd::Tui { prompt: None },
    };

    let mut positional: Vec<String> = Vec::new();
    let mut i = 0;
    let mut sub: Option<String> = None;
    while i < args.len() {
        let a = args[i].clone();
        let next = |i: usize| -> Option<String> { args.get(i + 1).cloned() };
        match a.as_str() {
            "--version" | "-V" | "-v" => opts.cmd = Cmd::Version,
            "--help" | "-h" | "-?" => opts.cmd = Cmd::Help,
            "--base" | "--url" => match next(i) {
                Some(v) => {
                    opts.base = v;
                    i += 1;
                }
                None => opts.cmd = Cmd::BadUsage("--base 后面要跟地址".into()),
            },
            "--token" => match next(i) {
                Some(v) => {
                    opts.token = Some(v);
                    i += 1;
                }
                None => opts.cmd = Cmd::BadUsage("--token 后面要跟值".into()),
            },
            "--session" | "-s" => match next(i) {
                Some(v) => {
                    opts.session = v;
                    i += 1;
                }
                None => opts.cmd = Cmd::BadUsage("--session 后面要跟会话名".into()),
            },
            "--mode" | "-m" => match next(i) {
                Some(v) => match Mode::parse(&v) {
                    Some(m) => {
                        opts.mode = m;
                        i += 1;
                    }
                    None => {
                        opts.cmd = Cmd::BadUsage(format!(
                            "--mode 只接受 agent/direct/side/battle，收到 {}",
                            v
                        ))
                    }
                },
                None => opts.cmd = Cmd::BadUsage("--mode 后面要跟模式".into()),
            },
            "--models" => match next(i) {
                Some(v) => {
                    opts.models = v
                        .split(',')
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .take(2)
                        .collect();
                    i += 1;
                }
                None => opts.cmd = Cmd::BadUsage("--models 后面要跟 a,b".into()),
            },
            "--no-logo" => opts.logo = false,
            "--logo" => opts.logo = true,
            "--reasoning" | "-r" => opts.reasoning = true,
            "--no-reasoning" => opts.reasoning = false,
            "--no-color" => opts.color = false,
            "--color" => opts.color = true,
            other if other.starts_with('-') && other.len() > 1 => {
                opts.cmd = Cmd::BadUsage(format!("不认识的选项：{}", other))
            }
            other => {
                if positional.is_empty() && sub.is_none() && !matches!(opts.cmd, Cmd::Version | Cmd::Help)
                {
                    if matches!(other, "health" | "models" | "resume" | "history" | "chat" | "tui")
                    {
                        sub = Some(other.to_string());
                        i += 1;
                        continue;
                    }
                }
                positional.push(other.to_string());
            }
        }
        i += 1;
    }

    let joined = positional.join(" ");
    let prompt = if joined.trim().is_empty() {
        None
    } else {
        Some(joined)
    };

    if matches!(opts.cmd, Cmd::Version | Cmd::Help | Cmd::BadUsage(_)) {
        return opts;
    }
    opts.cmd = match sub.as_deref() {
        Some("health") => Cmd::Health,
        Some("models") => Cmd::Models,
        Some("history") => Cmd::History {
            session: opts.session.clone(),
        },
        Some("resume") => {
            // resume <会话名> [问题]
            let mut parts: Vec<String> = positional.clone();
            if parts.is_empty() {
                Cmd::BadUsage("resume 后面要跟会话名：arena-lite resume main".into())
            } else {
                let session = parts.remove(0);
                let rest = parts.join(" ");
                opts.session = session.clone();
                Cmd::Resume {
                    session,
                    prompt: if rest.trim().is_empty() {
                        None
                    } else {
                        Some(rest)
                    },
                }
            }
        }
        Some("chat") | Some("tui") => Cmd::Tui { prompt },
        _ => match prompt {
            Some(p) => Cmd::OneShot { prompt: p },
            None => Cmd::Tui { prompt: None },
        },
    };
    opts
}

fn usage() -> String {
    let mut s = String::new();
    s.push_str("arena-lite ");
    s.push_str(VERSION);
    s.push_str(" —— arena_bridge 的终端客户端\n\n");
    s.push_str("用法：\n");
    s.push_str("  arena-lite                        进 TUI（交互式对话）\n");
    s.push_str("  arena-lite \"你好\"                 单发，流式输出\n");
    s.push_str("  arena-lite --mode side \"对比一下\"   指定模式单发\n");
    s.push_str("  arena-lite health                 体检桥\n");
    s.push_str("  arena-lite models                 列出模型\n");
    s.push_str("  arena-lite history [会话]          打印网页端历史\n");
    s.push_str("  arena-lite resume <会话> [问题]     载入该会话历史（有终端就进 TUI）\n\n");
    s.push_str("选项：\n");
    s.push_str("  -m, --mode <agent|direct|side|battle>   常驻模式，默认 agent\n");
    s.push_str("  -s, --session <名字>                    会话名，默认 main\n");
    s.push_str("      --models <a,b>                      双栏两栏用的模型\n");
    s.push_str("      --base <地址>                       桥地址，默认 127.0.0.1:17892\n");
    s.push_str("      --token <值>                        桥设了 ARB_TOKEN 时必填\n");
    s.push_str("      --reasoning                         思考链默认展开\n");
    s.push_str("      --no-logo                           跳过启动 LOGO\n");
    s.push_str("      --no-color                          关掉颜色\n");
    s.push_str("  -V, --version                           版本\n");
    s.push_str("  -h, --help                              本帮助\n\n");
    s.push_str("环境变量：ARENA_BASE / ARENA_TOKEN（也认 ARB_TOKEN）/ ARENA_SESSION /\n");
    s.push_str("          ARENA_MODE / ARENA_REASONING=1 / ARENA_NO_LOGO=1 / NO_COLOR=1\n");
    s
}

/* ------------------------------ 入口 ------------------------------ */

fn main() {
    term::init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let opts = parse_args(args);

    let code = match &opts.cmd {
        Cmd::Version => {
            println!("arena-lite {}", VERSION);
            0
        }
        Cmd::Help => {
            println!("{}", usage());
            0
        }
        Cmd::BadUsage(msg) => {
            eprintln!("参数有误：{}", msg);
            eprintln!();
            eprintln!("{}", usage());
            2
        }
        Cmd::Health => cmd_health(&opts),
        Cmd::Models => cmd_models(&opts),
        Cmd::History { session } => cmd_history(&opts, session),
        Cmd::OneShot { prompt } => cmd_oneshot(&opts, None, prompt.clone()),
        Cmd::Resume { session, prompt } => match prompt {
            Some(p) => match load_history_msgs(&opts, session) {
                Ok(pre) => cmd_oneshot(&opts, Some(pre), p.clone()),
                Err(msg) => {
                    eprintln!("{}", msg);
                    eprintln!("这条会话没读回来，就不接着聊了（不然等于假装有上下文）。");
                    1
                }
            },
            None => {
                if term::is_tty() {
                    let pre = match load_history_msgs(&opts, session) {
                        Ok(v) => v,
                        Err(msg) => {
                            eprintln!("{}", msg);
                            eprintln!("仍然进入 TUI，但不会载入历史。");
                            Vec::new()
                        }
                    };
                    run_tui(&opts, Some(session.clone()), Some(pre))
                } else {
                    cmd_history(&opts, session)
                }
            }
        },
        Cmd::Tui { prompt } => match prompt {
            Some(p) => cmd_oneshot(&opts, None, p.clone()),
            None => run_tui(&opts, None, None),
        },
    };
    std::process::exit(code);
}

fn build_client(opts: &Opts) -> Result<Client, String> {
    let base: Base = httpc::parse_base(&opts.base)?;
    Ok(Client::new(base, opts.token.clone()))
}

fn theme_for_stdout(opts: &Opts) -> Theme {
    Theme::new(opts.color && std::io::stdout().is_terminal())
}

/// 思考行加前缀；桥自己已经带了「· 」的就不重复加
fn reason_line(raw: &str) -> String {
    let t = raw.trim_end();
    if t.trim_start().starts_with('·') {
        t.trim_start().to_string()
    } else {
        format!("· {}", t)
    }
}

/* ------------------------------ 只读命令 ------------------------------ */

fn cmd_health(opts: &Opts) -> i32 {
    let client = match build_client(opts) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("参数有误：{}", e);
            return 2;
        }
    };
    match client.health() {
        Ok(h) => {
            println!("桥      : {}  {}", client.base.display(), if h.ok { "● 通" } else { "○ 不通" });
            println!("Chrome  : {}", if h.chrome { "● 已连接" } else { "○ 未启动" });
            println!("版本    : {}", if h.version.is_empty() { "?" } else { &h.version });
            println!("运行    : {} 秒", h.uptime);
            println!("队列    : {}/{}（同一会话最小间隔 {} ms）", h.queue, h.max_queue, h.min_gap_ms);
            println!("工作中  : {}", if h.working { "是" } else { "否" });
            println!(
                "会话    : {}",
                if h.sessions.is_empty() {
                    "（无）".to_string()
                } else {
                    h.sessions.join(" / ")
                }
            );
            println!("鉴权    : {}", if h.auth { "已开启 ARB_TOKEN" } else { "未开启" });
            if !h.chrome {
                println!();
                println!("Chrome 没连上：跑 `schtasks /run /tn chrome-arena`，或手动带 --remote-debugging-port=9222 启动。");
            }
            0
        }
        Err(e) => {
            eprintln!("{}", e.human());
            1
        }
    }
}

fn cmd_models(opts: &Opts) -> i32 {
    let client = match build_client(opts) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("参数有误：{}", e);
            return 2;
        }
    };
    match client.models() {
        Ok(list) => {
            for m in list {
                println!("{}", m);
            }
            0
        }
        Err(e) => {
            eprintln!("{}", e.human());
            1
        }
    }
}

fn cmd_history(opts: &Opts, session: &str) -> i32 {
    let client = match build_client(opts) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("参数有误：{}", e);
            return 2;
        }
    };
    let th = theme_for_stdout(opts);
    match client.history(session) {
        Ok(h) => {
            if h.messages.is_empty() {
                println!("会话 {} 里没读到消息", h.session);
                return 0;
            }
            for m in &h.messages {
                let role = m.role.to_ascii_lowercase();
                if role.contains("user") {
                    println!("{}", th.paint("你 ›", Palette::User));
                } else {
                    println!("{}", th.paint("arena ❯", Palette::Accent));
                }
                for line in m.text.split('\n') {
                    println!("  {}", line);
                }
                println!();
            }
            0
        }
        Err(e) => {
            eprintln!("{}", e.human());
            1
        }
    }
}

/// 把网页端历史读成消息列表。
///
/// 读失败时**不**假装成功（明确返回 Err，由调用方决定是退出还是照常开始），
/// 这也对应「桥侧读不到就读不到，客户端不假装支持」的要求。
fn load_history_msgs(opts: &Opts, session: &str) -> Result<Vec<ChatMsg>, String> {
    let client = match build_client(opts) {
        Ok(c) => c,
        Err(e) => return Err(format!("参数有误：{}", e)),
    };
    match client.history(session) {
        Ok(h) => {
            let mut out = Vec::new();
            for m in h.messages {
                if m.role.to_ascii_lowercase().contains("user") {
                    out.push(ChatMsg::user(m.text));
                } else {
                    out.push(ChatMsg::assistant(m.text));
                }
            }
            if out.is_empty() {
                return Err(format!("会话 {} 的历史是空的（页面里没读到消息）", session));
            }
            Ok(out)
        }
        Err(e) => Err(e.human()),
    }
}

/* ------------------------------ 单发 ------------------------------ */

fn cmd_oneshot(opts: &Opts, preload: Option<Vec<ChatMsg>>, prompt: String) -> i32 {
    let client = match build_client(opts) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("参数有误：{}", e);
            return 2;
        }
    };
    let th = theme_for_stdout(opts);
    let mut msgs: Vec<ChatMsg> = preload.unwrap_or_default();
    msgs.push(ChatMsg::user(prompt));
    let mut chat_opts = ChatOpts::new(opts.mode, opts.session.clone());
    chat_opts.models = opts.models.clone();

    let mut attempt = 0u32;
    loop {
        attempt += 1;
        let mut reason_pending = String::new();
        let mut printed_reason = false;
        let want_reason = opts.reasoning;
        let mut sink = |ev: StreamEvent| -> bool {
            match ev {
                StreamEvent::Text(t) => {
                    if printed_reason {
                        eprintln!();
                        printed_reason = false;
                    }
                    print!("{}", t);
                    let _ = std::io::stdout().flush();
                }
                StreamEvent::Reasoning(t) => {
                    reason_pending.push_str(&t);
                    loop {
                        let nl = match reason_pending.find('\n') {
                            Some(p) => p,
                            None => break,
                        };
                        let line: String = reason_pending[..nl].to_string();
                        reason_pending.drain(..=nl);
                        if !line.trim().is_empty() && want_reason {
                            eprintln!("{}", th.paint(&reason_line(&line), Palette::Reasoning));
                            printed_reason = true;
                        }
                    }
                    // 半行先攥着；实在太长就别憋了
                    if reason_pending.chars().count() > 400 {
                        let line = std::mem::take(&mut reason_pending);
                        if want_reason {
                            eprintln!("{}", th.paint(&reason_line(&line), Palette::Reasoning));
                            printed_reason = true;
                        }
                    }
                }
                StreamEvent::Tool { name, input } => {
                    if !reason_pending.trim().is_empty() && want_reason {
                        eprintln!(
                            "{}",
                            th.paint(&reason_line(&reason_pending), Palette::Reasoning)
                        );
                        printed_reason = true;
                    }
                    reason_pending.clear();
                    eprintln!(
                        "{}",
                        th.paint(
                            &format!("▸ 工具 {}  {}", name, err::truncate(input.trim(), 80)),
                            Palette::Tool
                        )
                    );
                }
            }
            true
        };

        match client.chat_stream(&msgs, &chat_opts, &mut sink) {
            Ok(()) => {
                if printed_reason {
                    eprintln!();
                }
                println!();
                return 0;
            }
            Err(BridgeError::NeedVerify) if attempt == 1 => {
                eprintln!();
                eprintln!("{}", th.paint("需要人工验证，已切到浏览器；过完验证会自动重试一次。", Palette::Error));
                let _ = foreground::bring_browser_forward();
                std::thread::sleep(Duration::from_secs(6));
                continue;
            }
            Err(BridgeError::Aborted) => {
                eprintln!();
                return 130;
            }
            Err(e) => {
                eprintln!();
                eprintln!("{}", th.paint(&e.human(), Palette::Error));
                return 1;
            }
        }
    }
}

/* ------------------------------ TUI ------------------------------ */

fn run_tui(opts: &Opts, session_override: Option<String>, preload: Option<Vec<ChatMsg>>) -> i32 {
    if !term::is_tty() {
        eprintln!("TUI 需要一个真终端。单发请用：arena-lite \"你的问题\"");
        return 2;
    }
    let client = match build_client(opts) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("参数有误：{}", e);
            return 2;
        }
    };
    let client = Arc::new(client);
    let th = Theme::new(opts.color);

    let raw = match term::enter_raw() {
        Some(r) => r,
        None => {
            eprintln!("进不了原始模式（拿不到终端控制权），TUI 无法启动。");
            return 2;
        }
    };

    let (tx, rx) = channel::<Msg>();

    // 进备用屏幕
    let mut so = std::io::stdout();
    let _ = write!(so, "{}{}", theme::ALT_SCREEN_ON, theme::CLEAR_SCREEN);
    let _ = so.flush();

    // LOGO
    if opts.logo {
        let size = term::size();
        let _ = write!(so, "{}", ui::render_logo(size, &th));
        let _ = so.flush();
        // 按任意键或 1 秒后消失
        let start = Instant::now();
        let mut skipped = false;
        while start.elapsed() < Duration::from_millis(1000) {
            let left = Duration::from_millis(1000).saturating_sub(start.elapsed());
            match rx.recv_timeout(left.min(Duration::from_millis(120))) {
                Ok(Msg::Key(_)) => {
                    skipped = true;
                    break;
                }
                Ok(_) => continue,
                Err(_) => continue,
            }
        }
        let _ = skipped;
        let _ = write!(so, "{}{}", theme::CLEAR_SCREEN, theme::CURSOR_HIDE);
        let _ = so.flush();
    }

    let cfg = Config {
        base_display: client.base.display(),
        session: session_override.unwrap_or_else(|| opts.session.clone()),
        mode: opts.mode,
        models: opts.models.clone(),
        reasoning_open: opts.reasoning,
    };
    let mut app = App::new(Arc::clone(&client), tx.clone(), cfg);
    if let Some(hist) = preload {
        if !hist.is_empty() {
            for m in &hist {
                if m.role == "user" {
                    app.blocks.push(app::Block::User {
                        text: m.content.clone(),
                    });
                } else {
                    app.blocks.push(app::Block::Assistant {
                        text: m.content.clone(),
                    });
                }
            }
            app.log = hist;
            app.push_system("已载入历史，接着聊就行");
        }
    }
    app.push_system(format!(
        "连上 {} · 模式 {} · 会话 {}",
        client.base.display(),
        app.mode().label(),
        app.cfg.session
    ));

    // 读键线程
    term::spawn_input(tx.clone());
    // 健康检查轮询
    spawn_health_poll(Arc::clone(&client), tx.clone());

    let code = tui_loop(&mut app, rx, &th);

    let _ = write!(
        std::io::stdout(),
        "{}{}",
        theme::CURSOR_SHOW,
        theme::ALT_SCREEN_OFF
    );
    let _ = std::io::stdout().flush();
    drop(raw);
    code
}

fn spawn_health_poll(client: Arc<Client>, tx: std::sync::mpsc::Sender<Msg>) {
    let _ = std::thread::Builder::new()
        .name("arena-health".to_string())
        .spawn(move || loop {
            match client.health() {
                Ok(h) => {
                    if tx.send(Msg::Health(Ok(h))).is_err() {
                        return;
                    }
                }
                Err(e) => {
                    if tx.send(Msg::Health(Err(e))).is_err() {
                        return;
                    }
                }
            }
            std::thread::sleep(Duration::from_secs(10));
        });
}

fn tui_loop(app: &mut App, rx: Receiver<Msg>, th: &Theme) -> i32 {
    let mut out = std::io::stdout();
    let mut last_size = term::size();
    let mut dirty = true;

    loop {
        // 每轮最多等 60ms，保证 spinner 转起来
        match rx.recv_timeout(Duration::from_millis(60)) {
            Ok(msg) => {
                handle_msg(app, msg, &mut out);
                dirty = true;
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                break;
            }
        }

        if app.notice_expired() {
            app.notice = None;
            dirty = true;
        }

        let size = term::size();
        if size != last_size {
            last_size = size;
            dirty = true;
        }

        let animating = app.busy() || app.notice.is_some();
        if dirty || animating {
            let frame = ui::render(app, size, th);
            if write!(out, "{}", frame).is_err() {
                break;
            }
            if out.flush().is_err() {
                break;
            }
            dirty = false;
        }

        if app.quit {
            break;
        }
    }
    0
}

fn handle_msg(app: &mut App, msg: Msg, out: &mut std::io::Stdout) {
    match msg {
        Msg::Key(k) => app.handle_key(k),
        Msg::Stream { id, ev } => app.on_stream(id, ev),
        Msg::StreamEnd { id, result } => app.on_stream_end(id, result),
        Msg::NeedVerify { id } => {
            // 响一次终端铃 + 状态栏转红（正文提示由 app 负责）
            let _ = write!(out, "{}", theme::bell());
            let _ = out.flush();
            app.on_need_verify(id);
        }
        Msg::Health(res) => app.on_health(res),
        Msg::History(res) => app.on_history(res),
        Msg::SessionList(list) => app.on_session_list(list),
        Msg::Notice(text) => app.notice(text),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use app::HealthState;
    use client::Health;

    fn opts_of(args: &[&str]) -> Opts {
        parse_args(args.iter().map(|s| s.to_string()).collect())
    }

    #[test]
    fn no_args_enters_tui() {
        let o = opts_of(&[]);
        assert!(matches!(o.cmd, Cmd::Tui { prompt: None }));
        assert_eq!(o.mode, Mode::Agent);
    }

    #[test]
    fn prompt_is_one_shot() {
        let o = opts_of(&["你好", "世界"]);
        match o.cmd {
            Cmd::OneShot { prompt } => assert_eq!(prompt, "你好 世界"),
            other => panic!("期望单发，得到 {:?}", other),
        }
    }

    #[test]
    fn version_and_help() {
        assert!(matches!(opts_of(&["--version"]).cmd, Cmd::Version));
        assert!(matches!(opts_of(&["-V"]).cmd, Cmd::Version));
        assert!(matches!(opts_of(&["--help"]).cmd, Cmd::Help));
    }

    #[test]
    fn mode_option_and_flag() {
        let o = opts_of(&["--mode", "battle", "打一架"]);
        assert_eq!(o.mode, Mode::Battle);
        assert!(matches!(o.cmd, Cmd::OneShot { .. }));
        let bad = opts_of(&["--mode", "nope"]);
        assert!(matches!(bad.cmd, Cmd::BadUsage(_)));
    }

    #[test]
    fn subcommands() {
        assert!(matches!(opts_of(&["health"]).cmd, Cmd::Health));
        assert!(matches!(opts_of(&["models"]).cmd, Cmd::Models));
        match opts_of(&["resume", "设计", "接着聊"]).cmd {
            Cmd::Resume { session, prompt } => {
                assert_eq!(session, "设计");
                assert_eq!(prompt.as_deref(), Some("接着聊"));
            }
            other => panic!("期望 resume，得到 {:?}", other),
        }
        assert!(matches!(
            opts_of(&["resume"]).cmd,
            Cmd::BadUsage(_)
        ));
    }

    #[test]
    fn session_and_models_options() {
        let o = opts_of(&["-s", "proj1", "--models", "gpt,claude,extra", "问题"]);
        assert_eq!(o.session, "proj1");
        assert_eq!(o.models, vec!["gpt".to_string(), "claude".to_string()]);
    }

    #[test]
    fn unknown_option_is_rejected() {
        assert!(matches!(opts_of(&["--nope"]).cmd, Cmd::BadUsage(_)));
    }

    #[test]
    fn logo_and_color_switches() {
        assert!(!opts_of(&["--no-logo"]).logo);
        assert!(!opts_of(&["--no-color"]).color);
        assert!(opts_of(&["--reasoning"]).reasoning);
    }

    #[test]
    fn usage_mentions_env() {
        let u = usage();
        assert!(u.contains("ARENA_BASE"));
        assert!(u.contains("arena-lite resume"));
    }

    #[test]
    fn health_state_placeholder_compiles() {
        let h = Health::default();
        let _ = HealthState::Up(h);
    }
}
