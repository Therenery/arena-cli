# arena-lite

`arena_bridge`（`../arena_bridge.js`，监听 `127.0.0.1:17892`）的**独立轻量 Rust 客户端**。

零依赖、纯 std，编译出来是一个静态二进制，不需要装任何 crates：

```bash
cd bridge/arena-lite
cargo build --release          # 产物 target/release/arena-lite
# 放进 PATH 后可直接用；也可以软链成 arena-lite / arb
```

只做桥接对接这一件事，完整覆盖桥接的对话与运维接口，不带微信/QQ/飞书之类用不上的功能。

## 用法

```bash
arena-lite                      # 进对话 TUI（流式实时出字）
arena-lite "后室的实体怎么设计?" # 单发提问，打印后退出
arena-lite -d "用一句话解释熵"   # direct 模式（更快）
arena-lite --mode battle "问题" # 单发对战
arena-lite -s 设计 -r "分析这个仓库" # 指定会话 + 显示思考过程
arena-lite models               # 聊天模型 + 路由模型
arena-lite health               # 桥接体检（chrome / 队列 / 会话）
arena-lite debug                # 会话内部状态
arena-lite new [会话]           # 清空服务端会话历史
arena-lite repos                # 工作区可切换的仓库
arena-lite repo Therenery/xxx   # 切换仓库（等于开新对话）
arena-lite upload ./a.png ./b.md
arena-lite pr [编号] [--repo owner/name]
arena-lite battle "问题" [--models a,b]   # 单发对战
arena-lite vote a|b|tie|both              # 给上一轮对战投票
```

**模式是常驻、可随时切换的**（和 arena.ai 网页端的模式选择一样，不是一次性命令）：
TUI / 行模式 REPL 里用 `/agent`、`/direct`、`/battle [问题]`、`/side [问题]` 或
`/mode <agent|direct|battle|side>` 切换，之后所有提问都走当前模式直到再切换；
对战两栏的模型用 `/panemodels a,b` 设置，对战轮用 `/vote a|b|tie|both` 投票。
状态栏最左侧始终显示当前模式。

完整参数见 `arena-lite help`。

## 配置（环境变量）

| 变量 | 默认 | 作用 |
| --- | --- | --- |
| `ARENA_BRIDGE_URL` / `ARB_BRIDGE_URL` | `http://127.0.0.1:17892` | 桥接地址（`--base-url` 优先） |
| `ARENA_TOKEN` / `ARB_TOKEN` | 空 | `Authorization: Bearer`，对应桥接的 `ARB_TOKEN` |
| `ARENA_SESSION` | `main` | 默认会话名（`-s` 优先） |
| `ARENA_TIMEOUT` | `900` | 单次请求超时秒数（`--timeout` 优先） |
| `ARENA_REASONING` | 空 | `1`/`true` 时默认显示思考过程 |

## TUI

- 顶部状态栏：模型 / 会话 / 桥接地址 / 健康点 / 等待秒数（agent 任务 40s~5min，有 spinner）
- 底部输入行：Enter 发送，Ctrl+C 退出，Esc 清空，↑↓ 历史问题，PgUp/PgDn 滚动对话
- 桥接全局串行：上一轮没跑完时新问题自动排队，结束后依次发出
- 斜杠命令：`/model` `/session` `/new` `/models` `/health` `/repos` `/repo` `/reasoning` `/upload` `/battle` `/vote` `/clear` `/quit`
- 非 TTY 环境（管道、CI）自动退化为行模式 REPL

## 实现说明

- `src/main.rs` — 参数解析、子命令分发、单发模式
- `src/bridge.rs` — 桥接 HTTP API 的类型化客户端（`/v1/chat/completions` 流式 SSE + `/ask` 之外的全部运维端点）
- `src/http.rs` — 零依赖 HTTP/1.1 客户端（只支持 `http://`，自动解 chunked，按行读 SSE）
- `src/json.rs` — 极简 JSON 解析/序列化
- `src/tui.rs` — 终端 TUI（`stty` 开 raw mode，ANSI 渲染，双线程事件循环）

桥接本身只监听 127.0.0.1 明文 HTTP，所以客户端也不支持 https；远程访问请走现有的隧道方案。
