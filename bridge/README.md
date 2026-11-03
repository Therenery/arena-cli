# arena-bridge

把 [arena.ai](https://arena.ai) 的 Agent 模式接进本地工作流：一个跑在电脑上的拦截式桥接服务，加一套手机/终端侧的 CLI。

拦截式指不伪造请求、不碰 reCAPTCHA、不读取 cookie。桥接服务用 CDP 驱动一个真实的 Chrome 标签页，用真实键盘事件输入，旁路读取页面自己发出的流。

## 架构

```
客户端（aichat / Codex / 任意 OpenAI 兼容工具）
   │  HTTP 127.0.0.1:17892
   ▼
arena_bridge.js（node，无第三方依赖）
   ├─ POST /v1/chat/completions   OpenAI 兼容，支持 stream
   ├─ POST /ask                   简化接口
   ├─ GET  /health  /v1/models  /debug  /new
   │
   ├─ CDP → Chrome 9222
   │    ├─ Page.addScriptToEvaluateOnNewDocument 注入拦截脚本
   │    └─ 逐字符 keyDown + 真实鼠标点击（tiptap 只认这条路径）
   │
   └─ 旁路 clone 页面自己的 fetch 流
        ├─ direct：/nextjs-api/stream/create-evaluation   a0: 正文  ag: 思考  ad: 结束
        └─ agent ：/ai-proxy/realtime/v1/sessions/{id}/out  SSE batch → text-delta / reasoning-delta / tool-input-available / finish
```

## 组件

| 文件                                          | 作用                                                                                            |
| --------------------------------------------- | ----------------------------------------------------------------------------------------------- |
| `arena_bridge.js`                             | 主服务，监听 127.0.0.1:17892                                                                    |
| `arb.ps1`                                     | Windows 侧调用入口，收 base64 问题、写结果 JSON                                                 |
| `rd2.js`                                      | 结果回传，gzip + base64 绕开 ssh 中文乱码                                                       |
| `start_bridge.ps1` / `start-arena-chrome.cmd` | 启动脚本，供计划任务调用                                                                        |
| `cli/arena`                                   | 总入口：无参进 TUI（缺 codex 时自动退到 REPL），带问题走单发，另有 `models/sessions/new/doctor` |
| `cli/arena-chat`                              | REPL 启动器，建隧道 → 体检 → aichat                                                             |
| `cli/arena-send`                              | 单发实现                                                                                        |
| `cli/arena-tunnel`                            | 三路隧道（tsnc / IPv6 / 直连），成功即返回                                                      |
| `cli/arena-responses`                         | **未实现（空文件）**，Codex CLI 需要的 Responses 适配器，见「已知限制」                         |
| `lib/parse.js`                                | 帧解析纯函数（a0/ag/a2/a3/ad + `records[]`），有 `node:test` 覆盖                               |
| `test/`                                       | `npm test`，不需要 Chrome                                                                       |

## 快速开始

电脑侧（Windows，需 Chrome 与 node）：

```powershell
schtasks /run /tn chrome-arena     # Chrome 带 --remote-debugging-port=9222
schtasks /run /tn arena-bridge     # 桥接服务
curl.exe http://127.0.0.1:17892/health
```

任意 OpenAI 兼容客户端：

```
base_url = http://127.0.0.1:17892/v1
api_key  = 任意非空字符串
model    = arena-agent（或 arena-direct）
```

手机侧：

```bash
arena                    # 进 TUI（未装 codex 时自动退到 REPL）
arena "问题"              # 单发
arena -d "问题"           # Direct 模式
arena doctor             # 隧道 / 桥接 / Chrome / 模型 体检
```

## 配置

全部走环境变量，不设也能跑（默认值即作者机器上的路径）。

| 变量                   | 默认             | 作用                                                               |
| ---------------------- | ---------------- | ------------------------------------------------------------------ |
| `ARB_STATE_DIR`        | `F:/__AI__/work` | 仓库缓存、调试转储的目录                                           |
| `ARB_MAX_SESSIONS`     | `6`              | 常驻标签页上限，超了按最久未用淘汰                                 |
| `ARB_UPLOAD_ROOTS`     | 空（不限制）     | `/upload` 允许外发的目录白名单，`;`（Windows）或 `:` 分隔          |
| `ARB_SSE_PING_MS`      | `15000`          | SSE 心跳间隔，防长任务被客户端判空闲                               |
| `ARB_DEBUG_PROMPT`     | 关               | 设为 `1` 才把 prompt 正文写日志/落盘                               |
| `ARB_TOKEN`            | 空（不校验）     | 非空时所有接口（除 `/health`）要求 `Authorization: Bearer <token>` |
| `ARB_MAX_PROMPT_CHARS` | `6000`           | 单轮 prompt 字符上限，超出截断并在响应里标记                       |
| `ARB_MAX_HISTORY_KEYS` | `32`             | 桥内部保存的会话历史条数上限（按键 LRU）                           |

## 安全设计

| 项        | 做法                                                      |
| --------- | --------------------------------------------------------- |
| 凭据      | 不读取、不存储、不外发任何 cookie 或 token                |
| reCAPTCHA | 完全不碰，由页面自己生成                                  |
| 监听      | 只绑 127.0.0.1                                            |
| 并发      | 全局队列串行（同时只跑 1 个），排队上限 4；队列满返回 429 |
| 鉴权      | 可选 `ARB_TOKEN`；隧道场景建议开启                        |
| 取消      | 客户端断开后当前轮次立即停止，不再占着队列跑满 10 分钟    |
| 频率      | 两次请求最小间隔 3 秒                                     |
| 输入      | 真键盘事件，页面看到的是人                                |
| 重试      | 失败不自动重试，避免触发风控                              |
| 日志      | 只记会话 id、游标、长度，不记 prompt 正文                 |

## 已知限制

1. Agent 任务结束后页面进入 review 状态，输入框消失，需要点「继续工作」才能继续。
2. 单轮耗时 40 秒到 5 分钟，取决于任务复杂度。
3. 依赖页面结构：输入框是 ProseMirror（agent）或 textarea（direct），改版要跟着改选择器。
4. 每个会话对应一个标签页，长期使用要清理。
5. `cli/arena-responses` 仍是空文件，Codex TUI 那条链路跑不起来（`arena-codex` 会直接报清楚原因）。
6. `usage` 里的 token 数是字符数占位，不是真实 token；`tools` 目前只打印到思考区，没有映射成 `tool_calls`。
7. Arena 服务条款禁止以程序化方式访问服务，使用风险自负。

## arena-lite（Rust 轻量客户端）

`arena-lite/` 是一个**零依赖、纯 std 的 Rust CLI**，专门对接本桥接，不带任何用不上的功能：

```bash
cd arena-lite && cargo build --release   # 单个静态二进制，不用联网拉 crates
./target/release/arena-lite              # 进对话 TUI（流式）
./target/release/arena-lite "问题"       # 单发
./target/release/arena-lite help         # 全部命令（models/health/new/repos/repo/upload/pr/battle/vote）
```

配置走环境变量：`ARENA_BRIDGE_URL`（默认 `http://127.0.0.1:17892`）、`ARENA_TOKEN`/`ARB_TOKEN`、`ARENA_SESSION`、`ARENA_TIMEOUT`。详见 `arena-lite/README.md`。

## 开发

```bash
npm test        # 帧解析单元测试，纯 node，不需要 Chrome
```

要求 node ≥ 21（用到全局 `WebSocket`）。

## 许可

私有仓库，未授权分发。

## Codex TUI（可选）

Codex CLI 只认 Responses API，桥接旁边加了一个适配器 `cli/arena-responses`（监听 127.0.0.1:17893），把 Responses 转成 chat/completions。

```bash
arena-codex            # 自动建隧道、拉起适配器，进 Codex 的 TUI
```

provider 配置见 `codex/config.toml.example`：`wire_api = "responses"`、`base_url = http://127.0.0.1:17893/v1`、`env_key = "ARENA_API_KEY"`。

从 GitHub Releases 下载的非打包二进制要加 `--no-daemon`，否则报 `no complete local package`。

## 仓库切换

```bash
arena repos                    # 列出页面里可选的 GitHub 仓库
arena repo Therenery/Backrooms # 切换工作区仓库（等于开新对话）
arena chat -R Therenery/xxx    # 进 REPL 前先切
```

## Arena Code：Windows 终端界面（Qwen Code）

终端这一层用 Qwen Code 的 TUI 当外壳（Apache-2.0，自带 OpenAI 兼容 provider），只把它当键盘和显示器：提示词、工具调用一律不参与，真正干活的是 Arena 页面里的 Agent。

### 为什么不是 Codex

Codex 的 TUI 按固定宽度排版，Termux 88 列下会崩；Qwen Code 的 TUI 能自适应，且 packages/core/src/core/openaiContentGenerator/ 已经内置 OpenAI 兼容实现，接桥只需配 base_url。

### 客户端配置

%USERPROFILE%\.arena\settings.json：

```json
{
  "modelProviders": {
    "openai": [
      {
        "id": "arena-agent",
        "name": "Arena Agent",
        "baseUrl": "http://127.0.0.1:17892/v1",
        "envKey": "ARENA_API_KEY"
      }
    ]
  },
  "model": { "name": "arena-agent", "baseUrl": "http://127.0.0.1:17892/v1" },
  "security": { "auth": { "selectedType": "openai" } },
  "env": { "ARENA_API_KEY": "sk-arena-local" }
}
```

cli-windows\arena.cmd 放进 PATH（例如 E:\Programz\bin），敲 arena 即进界面：

- 体检 17892，没起来就拉起 arena_bridge.js
- ARENA_SYSTEM_MD 指向空文件，客户端不再自带系统提示词
- 工作目录切到空目录，避免把 AGENTS.md 之类带进对话

### 常用命令

| 命令               | 作用                                            |
| ------------------ | ----------------------------------------------- |
| `/repo`            | 列出 Arena 工作区里可切换的仓库                 |
| `/repo owner/name` | 切换到指定仓库（会重建会话）                    |
| `/upload <路径>`   | 把本地文件挂到 Arena 输入框，路径带空格时用引号 |

思考过程、工具调用、路由模型会以 `·` 开头的行显示在思考区。

### 发送机制（踩过的坑）

Arena 的输入框是 tiptap/ProseMirror。只有逐字符的真实键盘事件会更新它的内部状态：

| 方式                                       | 输入框有字 | 发送按钮                    | 能否发出 |
| ------------------------------------------ | ---------- | --------------------------- | -------- |
| Input.insertText                           | 是         | pointer-events:none、半透明 | 否       |
| execCommand('insertText')                  | 是         | 同上                        | 否       |
| Input.dispatchKeyEvent{type:'char'} 逐字符 | 是         | 可用                        | 是       |

所以 typePrompt() 逐字符发 char 事件，clickSend() 先看按钮 pointer-events 是否为 none，再用真实鼠标事件点它。

另外两个必须清掉的东西：

- 客户端的 system 消息：Arena 页面自带系统提示词，客户端再塞一份会互相打架，桥里直接丢弃。
- 客户端注入的上下文：Qwen Code 会往 user 消息里塞 --- Context from: ... --- 和 <system-reminder>，一次两万字，Arena 只会回一句「需要我做什么」。桥里用正则剥掉。

### 新会话

新会话就是导航到 https://arena.ai/agent （不带会话 id）。仓库/连接器选择是账号级状态，新会话自动继承，/repo 只在需要换库时用。

### 从源码构建（开源版）

```powershell
git clone --depth 1 https://github.com/QwenLM/qwen-code.git
cd qwen-code
git apply F:\path\to\arena-bridge\patches\arena-code.patch
corepack pnpm install
corepack pnpm run build
```

补丁做四件事：把内置 banner 换成 ARENA 字符画（青色→靛紫渐变）、标题改 Arena Code、清掉所有俏皮加载文案、程序名改成 arena。
