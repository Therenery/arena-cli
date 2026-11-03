# arena-lite

`arena_bridge` 的专用终端客户端：**零第三方依赖**（只有 std）、**Windows 优先**、单二进制。

只做桥接这一件事——常驻模式对话（agent / direct / side / battle）、双栏对战与投票、网页端历史载入、桥体检。
**没有**渠道（微信/QQ/飞书/钉钉）、**没有**沙箱、**没有** MCP、**没有**扩展/skills/子代理/批量任务。

```
 arena │ agent │ arena-agent │ main │ 127.0.0.1:17892 ●                  ⠋ 12s
──────────────────────────────────────────────────────────────────────────────────
你 › 帮我看看这个仓库的结构
arena ❯ 好的，我先看一下目录树……
· 思考（12 行，Ctrl+R 展开）
▸ 工具 read_file  src/main.rs
──────────────────────────────────────────────────────────────────────────────────
› 输入问题，Enter 发送，Ctrl+C 退出
```

---

## 1. 编译

Windows（目标产物 `target\release\arena-lite.exe`）：

```powershell
cd bridge\arena-lite
cargo build --release        # 零警告；只有 std，不会去 crates.io 拉任何东西
```

Linux / macOS：

```bash
cd bridge/arena-lite
cargo build --release        # 产物 target/release/arena-lite
```

没有 cargo 的环境（本沙箱就是）用等价脚本直编，因为依赖为零，`rustc` 一把过：

```bash
bash scripts/check.sh        # 编 release + 跑全部单测 + 顺手做一次 Windows 目标编译检查
```

### 是否真的编译过？——编过，说明清楚

| 项 | 状态 |
| --- | --- |
| 宿主（x86_64-unknown-linux-gnu）`rustc 1.88.0` 编译 | ✅ 通过，**零警告** |
| 单元测试 | ✅ **122 个全过**（见第 9 节） |
| `--target x86_64-pc-windows-gnu` 完整编译（`--emit=obj`，含全部 `cfg(windows)` 分支） | ✅ 通过，零警告，产出 COFF 目标文件 |
| 链接出 `arena-lite.exe` | ❌ 沙箱里**没做到**：缺 mingw 导入库（`libkernel32.a`/`libuser32.a` 等），而这台机器只允许访问 npm/pypi/github 镜像，拿不到 mingw-w64 工具链。代码没问题（编译到目标文件那一步已经过了），是链接器找不到系统库 |
| `cargo` | ❌ 沙箱里没有 cargo，也没有 crates.io 网络；本 crate 零依赖，所以用 `rustc` 直编等价 |
| 端到端 | ✅ 起假桥跑通单发/双栏/409 验证重试/队列满/断连/history/resume，TUI 用伪终端跑通 19 项交互检查 |

> 一句话：**Linux 上真编过、真跑过；Windows 目标真编译到目标文件、但沙箱里没链接成 exe。**
> 你在 Windows 上跑一次 `cargo build --release` 即可拿到单文件 `arena-lite.exe`。

终端层的 Windows 代码只用 Win32 控制台 API，**没有任何 `Command::new` 调用**（上一版用 `stty` 就是这么挂的）：

- 输入：`SetConsoleMode`（开 `ENABLE_VIRTUAL_TERMINAL_INPUT`，关 `ENABLE_LINE_INPUT` / `ENABLE_ECHO_INPUT` / `ENABLE_PROCESSED_INPUT`）+ `ReadConsoleInputW` 读真实键记录（所以 Shift+Enter 能和 Enter 分开）
- 输出：`ENABLE_VIRTUAL_TERMINAL_PROCESSING` + `SetConsoleOutputCP(65001)`
- 尺寸：`GetConsoleScreenBufferInfo` 的 `srWindow`
- 验证时拉窗口：`EnumWindows` + `ShowWindow` + `SetForegroundWindow`（`AttachThreadInput` 兜策略）+ `FlashWindowEx`

Unix 侧才用 `termios` / `ioctl(TIOCGWINSZ)` / `poll`，也是自己声明的 FFI，不依赖 libc crate，更不调 `stty`。

---

## 2. 运行

```bash
arena-lite                          # 进 TUI，agent 模式，会话 main
arena-lite "你好"                    # 单发，流式打印
arena-lite --mode side "对比一下"     # 指定模式单发
arena-lite --mode battle "打一架"     # 对战（双栏合一）
arena-lite health                   # 桥体检
arena-lite models                   # 模型列表
arena-lite history main             # 打印网页端历史
arena-lite resume main              # 载入 main 的网页端历史；有终端就进 TUI
arena-lite resume main "接着说"       # 带着历史单发一条
arena-lite --version
```

常用选项：

| 选项 | 说明 |
| --- | --- |
| `-m, --mode <agent\|direct\|side\|battle>` | 常驻模式，默认 `agent` |
| `-s, --session <名字>` | 会话名，默认 `main`（对应桥的 `user` 字段） |
| `--models <a,b>` | battle/side 两栏各自的模型（最多两个） |
| `--base <地址>` | 桥地址，默认 `127.0.0.1:17892`；也接受 `http://127.0.0.1:17892` |
| `--token <值>` | 桥设了 `ARB_TOKEN` 时必填 |
| `-r, --reasoning` | 思考链默认展开 |
| `--no-logo` | 跳过启动 LOGO |
| `--no-color` | 关颜色 |
| `-V/--version`、`-h/--help` | 版本 / 帮助 |

子命令可以放在选项前后：`arena-lite --base 1.2.3.4:1 health`。

### 环境变量

| 变量 | 作用 |
| --- | --- |
| `ARENA_BASE` | 桥地址（默认 `127.0.0.1:17892`） |
| `ARENA_TOKEN` / `ARB_TOKEN` | 桥的 Bearer token（`ARB_TOKEN` 直接沿用桥那边的变量名） |
| `ARENA_SESSION` | 默认会话名 |
| `ARENA_MODE` | 默认模式 |
| `ARENA_REASONING=1` | 思考链默认展开（等价 `--reasoning`） |
| `ARENA_NO_LOGO=1` | 跳过 LOGO |
| `NO_COLOR=1` / `ARENA_NO_COLOR=1` | 关颜色 |
| `COLUMNS` / `LINES` | 拿不到终端尺寸时的兜底（Unix） |

---

## 3. 键位

| 键 | 行为 |
| --- | --- |
| `Enter` | 发送 |
| `Shift+Enter` / `Alt+Enter` / `Ctrl+J` | 换行（Windows 上 Shift+Enter 靠 `ReadConsoleInputW` 的修饰键状态，真能分开；多数 Unix 终端把 Shift+Enter 和 Enter 发成同一个字节，请用 Alt+Enter 或 Ctrl+J） |
| `Ctrl+C` | 空闲时退出；流式中第一次**中断生成**，再按一次退出 |
| `Esc` | 清空输入 |
| `↑` / `↓` | 输入历史（多行输入时不抢这两个键） |
| `PgUp` / `PgDn` | 滚动对话区 |
| `Ctrl+R` | 展开/收起思考链 |
| `Ctrl+T` | 展开/收起工具调用块 |
| `Ctrl+L` | 清屏（只清屏，服务端上下文保留） |
| `Tab` | 补全斜杠命令（唯一匹配直接补全，多个则补公共前缀） |
| `Ctrl+D` | 退出 |

滚动跟随规则：**贴底时自动跟随**；一旦 `PgUp` 上滚就停止跟随（新内容不会把你正在看的位置顶走），
对话区右下角出现 `↓ 新内容`；滚回底部、或直接发送新消息时恢复跟随。

---

## 4. 斜杠命令

| 命令 | 作用 |
| --- | --- |
| `/agent` `/direct` `/side` | 切常驻模式，之后所有回合都沿用 |
| `/battle [问题]` | 切到对战；带问题就直接用 battle 发出去 |
| `/mode <agent\|direct\|side\|battle>` | 同上，显式写法 |
| `/models [a,b]` | 查看 / 设置双栏两栏模型 |
| `/session <名字>` | 切会话名 |
| `/resume [会话]` | 不带参数：列出可用会话（名字 + 最后一条摘要 + 上次活动时间 + 条数），输入编号即载入；带参数直接载入 |
| `/sessions` | 同 `/resume` |
| `/new` | 清空服务端会话（`GET /new`） |
| `/health` | 体检桥（`GET /health`） |
| `/repos` `/repo <owner/name>` | 查看 / 切换工作区仓库（切仓库等于开新对话） |
| `/vote a\|b\|tie\|both` | 对战投票（`POST /vote`） |
| `/upload <路径[,路径]>` | 上传文件到当前会话（`POST /upload`） |
| `/reasoning` `/tools` | 展开/折叠思考链 / 工具块 |
| `/clear` | 清屏 |
| `/retry` | 重发上一问（嗯，第 7 条里的「最多重试一次」用完后可以用它手动再来） |
| `/help` | 命令表 |
| `/quit` `/exit` | 退出 |

---

## 5. 和桥的协议对接

普通对话**只走** `POST /v1/chat/completions`，不碰 `/battle`（那是一次性的）：

```json
{
  "model": "arena-agent",
  "messages": [{ "role": "user", "content": "..." }],
  "stream": true,
  "user": "main",
  "metadata": { "mode": "agent", "models": [] }
}
```

- `metadata.mode` ∈ `agent` | `direct` | `side` | `battle`，缺省 `agent`
- `metadata.models` 最多两个，`side` / `battle` 两栏用
- `user` 是会话名（桥按它分标签页和历史）

SSE 侧：逐行读 `data: {...}`，`data: [DONE]` 结束；`choices[0].delta.content` 是正文，
`delta.reasoning_content` 是思考。**工具调用**桥目前是把 `· 工具名 {json}` 这样一行塞在
`reasoning_content` 里，客户端按行拆回「工具块」（同时也兼容标准 `delta.tool_calls`，桥以后改格式不用动客户端）。
心跳（`: ping` 注释行）会被安静丢弃，所以长时间工具调用不会误判超时。

一次对话最多回传最近 12 条消息（`MAX_HISTORY_MSGS`），桥那边需要回放上下文时够用又不会把 prompt 撑爆。

---

## 6. 需要人工验证时（NEED_VERIFY / 409）

一条链路三方都要动，否则用户根本看不见验证框：

1. **桥侧**（`arena_bridge.js`）：检测到 `VERIFY_JS` 命中就调 CDP 的 `Page.bringToFront`，把那个标签页切到前台（同一标签页 1.5 秒内不重复调，避免轮询里抢焦点）。
2. **客户端正文 + 状态栏**：收到 409 或响应体含 `NEED_VERIFY` → 状态栏转红显示 `⚠ 需要人工验证`，正文插入红色醒目提示「需要人工验证，已切到浏览器」，并**响一次终端铃**（`\x07`）。
3. **Windows 再补一步**：`SetForegroundWindow` + `FlashWindowEx` 闪任务栏（按窗口标题找 Chrome/arena 主窗口；非 Windows 平台是空操作）。
4. **自动重试一次**：验证类错误不立刻报死，等 6 秒后自动重发同一请求（最多一次）；仍然要验证才把人话错误交回去，并可以 `/retry` 手动再来。

---

## 7. resume：把网页端历史灌回对话区

客户端侧：

```bash
arena-lite resume main            # 有终端 → 进 TUI 并载入历史
arena-lite resume main "接着说"    # 单发，历史会作为 messages 一起送
```

TUI 里 `/resume` 会列出桥 `/health` 里的会话，逐个用 `GET /history?session=<name>` 读摘要（最后一条 + 条数 + 上次活动时间），输入编号即载入。

桥侧新增的是只读端点（`arena_bridge.js` 0.4.0）：

```
GET /history?session=<name>
→ { ok, session, at, turns, via, messages:[{role, text}] }
```

`via` 说明它用哪层启发式读的 DOM：`role-attrs`（最准）/ `bubble-align`（按气泡靠右判用户）/ `alternate`（交替猜，客户端会额外提示「顺序是猜的」）。
**读不到就明确失败**：返回 `{ok:false,error:...}` 或 404，客户端会打印原因并以非 0 退出，不会假装支持、也不假装有上下文。

---

## 8. 错误翻译

| 情况 | 客户端说什么 |
| --- | --- |
| 需要人机验证（409 / `NEED_VERIFY`） | 需要人工过人机验证。已把 Chrome 切到前台……完成后自动重试一次 |
| 队列满（429 / `busy: queue full`） | 桥的队列满了：最多同时 4 个任务，同一会话两次提问至少间隔 3 秒…… |
| 鉴权失败（401/403） | 桥设了 `ARB_TOKEN`，客户端没带或对不上；用 `--token` 或 `ARENA_TOKEN` |
| Chrome 没起（500 `chrome unavailable`） | Chrome 没在跑（9222 端口连不上）；`schtasks /run /tn chrome-arena` 或手动带 `--remote-debugging-port=9222` |
| 超时（408/504/读超时） | 等太久没动静……去 Chrome 看一眼标签页，Ctrl+C 可中断 |
| 连不上桥 | 连不上桥 `127.0.0.1:17892`……先跑 `node bridge/arena_bridge.js` |
| 其它 HTTP 错误 | `桥返回 HTTP <code>：<桥的原话>` + 一句该怎么办 |

所有提示都是「发生了什么 + 现在该干什么」，没有 `unwrap()` 会崩在用户输入路径上（全库 0 个 `unwrap`/`expect`，测试里也没有）。

---

## 9. 测试

### 单元测试：122 个，`cargo test` 或 `bash scripts/check.sh`

覆盖要求里点名的四类，且每类都不是走过场：

| 模块 | 覆盖内容 | 数量 |
| --- | --- | --- |
| `ui` | 帧行数=终端行数、任意宽度不越界、状态栏六要素、spinner+秒数、折叠/展开文案、`↓ 新内容`、验证态转红、空态、极小终端不 panic、LOGO、工具摘要 | 11 |
| `app` | 输入编辑（含中英混合光标）、历史、多行、模式命令、Tab 补全、滚动跟随/回底恢复、流式块累积、旧回合丢弃、错误落块、相对时间 | 15 |
| `keys` | 方向键、PgUp/PgDn、Ctrl 组合、UTF-8 分片、ESC 序列分片、CSI-u 的 Shift+Enter、Tab/BackTab、未知序列不 panic | 14 |
| `width` | CJK/假名/谚文宽 2、组合符号宽 0、emoji 宽 2、按显示宽度折行（不切断宽字符）、混合中英、超长词强断、制表符/CRLF、任意宽度不越界 | 12 |
| `sse` | 单/多事件、跨分片拼帧、CRLF、多行 data、注释心跳、`[DONE]`、`event:` 字段、收尾残留、UTF-8 被切开的字节、逐字节喂入 | 12 |
| `json` | 嵌套对象/数组、`\uXXXX` 与代理对、转义、数字、空容器、坏输入不 panic、深层嵌套不爆栈、往返转义、真实 chunk 形状 | 11 |
| `main` | 参数解析（无参→TUI、提示词→单发、模式、子命令、非法选项、logo/color 开关） | 10 |
| `client` | 请求体（mode/models/转义）、health 解析、思考/工具行拆分（含跨分片与误判防护） | 9 |
| `httpc` | base 地址解析（裸 host、`http://`、前缀、IPv6、默认端口、拒绝 https/垃圾输入）、传输分块判定 | 7 |
| `err` | 409/429/401/chrome/超时的分类、中文文案含处置建议、错误体抽取、按字符截断 | 7 |
| `urlenc` | unreserved 保留、空格 `%20`、CJK 按 UTF-8 字节编码、保留字转义、query 拼接 | 6 |
| `theme` | 配色开关、四种模式颜色互不相同、LOGO 形状、定位/铃铛序列 | 4 |
| `mode` | 别名解析、传给桥的字面量、双栏判定 | 3 |
| `term/unix` | termios 偏移量守卫 | 1 |

### 端到端：`bash scripts/e2e.sh build/arena-lite`

自带一个假桥（`scripts/mock_bridge.js`，Node，不需要 Chrome），验证：

- 单发流式正文、`--reasoning` 下工具行拆成工具块、`--mode battle` 双栏
- `health` / `history` / `resume`（含「读不到就非 0 退出」）
- 429 队列满、桥没起来 —— 都是中文人话
- 409 验证 → 等 6 秒 → 自动重试成功（脚本会另起一个 `VERIFY_TIMES=1` 的假桥）
- TUI：伪终端驱动 19 项，包括状态栏内容、流式渲染、Ctrl+R / Ctrl+T 折叠、`/battle` 双栏、`↓ 新内容`、`/help`、Tab 补全、`/resume`、流式中 Ctrl+C 只中断不退出、再按 Ctrl+C 退出

---

## 10. 目录

```
bridge/arena-lite/
├── Cargo.toml            # 无 [dependencies]，就是这个 crate 的全部依赖声明
├── README.md
├── scripts/
│   ├── check.sh          # 没有 cargo 时的等价构建 + 测试
│   ├── mock_bridge.js    # 假桥（node，无 Chrome）
│   ├── tui_smoke.py      # 伪终端驱动 TUI
│   └── e2e.sh            # 一键跑上面两个
└── src/
    ├── main.rs           # 参数、子命令、单发、TUI 主循环
    ├── app.rs            # 状态机：输入编辑/斜杠命令/流式回合/滚动跟随
    ├── ui.rs             # 渲染：状态栏/对话区/输入区/LOGO
    ├── client.rs         # 桥客户端：SSE、事件分发、工具行拆分
    ├── httpc.rs          # 手写 HTTP/1.1（identity + chunked）
    ├── sse.rs            # SSE 分帧 + 增量 UTF-8 解码
    ├── json.rs           # 最小 JSON 解析/转义
    ├── err.rs            # 错误 → 中文人话
    ├── term/{mod,unix,windows}.rs  # 终端层，按平台分派
    ├── keys.rs           # 按键模型 + VT 序列解析（Unix）
    ├── foreground.rs     # 把 Chrome 窗口拉到前台（Windows）
    ├── theme.rs          # 256 色调色板 / LOGO / 转义序列
    ├── width.rs          # CJK 宽度与折行
    ├── mode.rs           # agent/direct/side/battle
    └── urlenc.rs         # 百分号编码
```

---

## 11. 已知限制

- Windows 上 Shift+Enter 依赖 `ReadConsoleInputW` 的修饰键状态，**在 SSH/串口这类非控制台 stdin 下退化**（这时 `is_tty()` 会失败，TUI 会直接拒绝启动并给出提示，不会装作能跑）。
- 想在 TUI 里换桥地址得退出重进（`--base` 是启动参数）；`/session` 换会话名后本地对话历史会继续沿用（想彻底分开用 `/new` 或重开）。
- `resume` 的准确度取决于桥能不能从 DOM 里认出消息节点；最差情况（`via: alternate`）顺序是猜的，客户端会明说。
- 单发模式不支持「流式中 Ctrl+C 中断」（没有 raw mode，SIGINT 直接结束进程，桥侧会检测到客户端断开并停止生成）。
- 工具块展开显示**参数**；桥目前只旁路到 `tool-input-available`，没有工具**结果**事件，所以没有「结果」可显示（客户端已预留渲染位置，桥以后发就能直接显示）。

---

## 12. 桥侧改动（单独列一节，方便回滚）

只加不改，全部集中在 `bridge/arena_bridge.js` + 新增 `bridge/lib/history.js`：

1. **新增 `GET /history?session=<name>`**（只读）
   - 从页面 DOM 读回对话；三层启发式（`role-attrs` → `bubble-align` → `alternate`），匹配不上返回 `{ok:false,error}`。
   - 返回字段：`ok, session, at, turns, via, messages[{role,text}]`。
   - 只有 `sessions` 里存在的会话可读，没有就 404 `{ok:false,error:'no session',sessions:[...]}`。
2. **人机验证时调 `Page.bringToFront`**
   - 新增 `needVerify(c)`：限流 1.5 秒后 `Page.bringToFront`，失败只记日志不影响原有错误码。
   - 5 处 `throw new Error('NEED_VERIFY')` 换成 `throw await needVerify(c)`（`waitReady` / `askOnce` / 流式轮询 / `askBattle` 两处）。
3. **新增 `bridge/lib/history.js`**：把 `HISTORY_JS` 挪出来，配 `bridge/test/history.test.js` 用假 DOM 覆盖（7 个用例），页面结构变了先挂测试。
4. **版本号 `0.3.1` → `0.4.0`**（`arena_bridge.js` 的 `VERSION` 与 `package.json` 同步）。

**没动的东西**：现有端点的路径、请求体、返回字段、错误码、队列/节奏限制、`/battle` `/vote` `/upload` `/repos` `/new` 的行为全部保持原样。
回滚只需要：删掉 `/history` 路由 + `lib/history.js`、把 5 处 `await needVerify(c)` 换回 `new Error('NEED_VERIFY')`、版本号改回 `0.3.1`（客户端此时对 `resume` 会明确报 404/读不到）。
