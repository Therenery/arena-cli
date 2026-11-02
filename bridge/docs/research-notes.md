# Arena 第三方项目全景与安全评估（2026-10-30 二次调研）

上一版只看三个项目，这次把 GitHub / PyPI / npm 全扫了一遍。结论有变化：**有更值得抄的东西，而且发现了端点漂移这个致命问题。**

---

## 一、全景：同类项目分四类

| 项目                              | 星    | 语言   | 最后提交       | 许可      | 类别                 | 能不能自己改             |
| --------------------------------- | ----- | ------ | -------------- | --------- | -------------------- | ------------------------ |
| `foxhui/WebAI2API`                | 1412  | JS     | 2026-07-10     | —         | 浏览器驱动           | 能，但 Camoufox 重       |
| `lumingya/universal-web-api`      | 501   | Python | 2026-10-10     | AGPL      | 浏览器驱动（多站点） | 能，14 万行太大          |
| `CloudWaddie/LMArenaBridge`       | 410   | Python | 2026-08-29     | —         | 服务端桥接           | 能，46 万字符难读        |
| `deanxv/lmarena2api`              | 125   | Go     | 2025-06-01     | —         | 服务端桥接           | **废了**，模型表写死过期 |
| `flay-o/arena2api`                | 161   | JS     | 2026-03-10     | 无        | 浏览器扩展           | 能，1495 行，最干净      |
| `Gandhara2077/arena-local-bridge` | 41    | JS     | **2026-10-30** | MIT       | **拦截式**           | **推荐**                 |
| `jtostrings/LMarenaBridge`        | 102   | JS     | 2026-03-19     | —         | 桌面应用             | 有打包产物，审计成本高   |
| `SeanShaffer88/ArenaDesktop`      | 0     | C#     | 2026-10-27     | MIT       | Windows 客户端       | 要编译 .NET，star 0      |
| `PiBOH/Arena-AI`                  | —     | Kotlin | 2026-07-26     | Unlicense | 安卓 APK             | **闭源产物，排除**       |
| `DeepJH/VeryGoodLMArena`          | 2     | Java   | 2026-02-21     | MIT       | 安卓                 | 同上                     |
| `xtekky/gpt4free` 的 `Arena.py`   | 6 万+ | Python | **2026-10-08** | MIT       | 服务端桥接           | 739 行，可抄             |
| `chen-dahan/Arena.ai-Plus`        | 7     | JS     | 2026-03-16     | GPL       | Chrome 扩展          | 功能增强，非 API         |

PyPI 上的 `lmarena` 是个空壳（0.1.0，无描述）。npm 上的 `koishi-plugin-lmarena*` 都是 koishi 机器人的绘图插件，跟 API 无关。

---

## 二、安全性：先划红线

**直接排除的三类：**

1. **闭源二进制**——`deanxv/lmarena2api` 的 25MB 单文件 Go 二进制、`PiBOH/Arena-AI` 的 APK、`jtostrings/LMarenaBridge` 的打包桌面版。没法审计，而且它们的核心价值（模型 ID 表、端点）都已经过期。
2. **默认对外监听**——`flay-o/arena2api` 默认 `0.0.0.0:9090` 且 `API_KEY` 为空，局域网内谁都能用你的账号。改代码能救，但要记得改。
3. **带自更新或代码执行**——`lumingya/universal-web-api` 有从 GitHub Release 拉 zip 覆盖自身文件的更新器，还有 `exec_command` 引擎（`command_engine_actions.py:3107`）。作者自己在 `docs/SECURITY-TRUST-BOUNDARIES.md` 承认"运行时装解析器等于源码写进项目再 import，不是隔离"。`arena-local-bridge` 也有 `exec_command`，README 明确说别开 cloudflared 隧道暴露它。

**审计过的四个项目都没发现后门、遥测、混淆或他人密钥。** 风险都在设计面，不在恶意代码。

---

## 三、最有价值的发现：端点漂移

这是这轮调研最有用的东西。

| 项目                              | 更新时间   | 用的端点                                                                     |
| --------------------------------- | ---------- | ---------------------------------------------------------------------------- |
| `CloudWaddie/LMArenaBridge`       | 2026-08-29 | `/nextjs-api/stream/create-evaluation`                                       |
| `xtekky/gpt4free`                 | 2026-10-08 | `/nextjs-api/stream/create-evaluation`                                       |
| `Gandhara2077/arena-local-bridge` | 2026-10-30 | `/nextjs-api/stream/create-chat` + `/ai-proxy/realtime/v1/sessions/{id}/out` |

Arena 在 8 月到 10 月之间换了端点。**所有硬编码 `create-evaluation` 的实现，现在大概率 404。**

这直接推翻了我上一版文档里那套"自己实现协议"的方案。硬编码端点，Arena 每次改版都得跟着修，跟现在点 DOM 的困境是同一个病。

**拦截式方案天然免疫这个问题**：不构造请求，只在页面里接管 `window.fetch` / `XMLHttpRequest` / `EventSource`，旁路 clone 一份读。页面自己发什么、往哪发、带什么 token，全由页面负责，脚本只当旁观者。

`arena-local-bridge` 的 `src/probe/modules/interceptor.js`（575 行）就是干这个的：

```js
// 第 12 行注释
// 所以我们在页面最早的时机接管 fetch / XHR / EventSource，旁路读取（clone）
```

它的判定逻辑（`interceptor.js:84`）：

```js
if (
  url.origin !== location.origin ||
  !(
    /^\/ai-proxy\/realtime\/v1\/sessions\/[^/]+\/out$/.test(url.pathname) ||
    url.pathname === '/nextjs-api/stream/create-chat'
  )
)
  return;
```

拦截之后按 Vercel AI SDK 的帧类型解析（`a0:` / `ag:` / `ad:`），这套帧格式从 8 月到现在没变过。

---

## 四、推荐的基座：arena-local-bridge

`Gandhara2077/arena-local-bridge`，★41，MIT，v1.1.0，今天（2026-10-30）还在提交。

| 项       | 值                                                                              |
| -------- | ------------------------------------------------------------------------------- |
| 依赖     | **只有 `playwright` 一个**（`package.json`）                                    |
| Node     | ≥ 20                                                                            |
| 绑定     | `127.0.0.1`，不开公网                                                           |
| 代码量   | 6860 行，按功能分 15 个模块，最大 `interceptor.js` 575 行                       |
| 文档     | README 中英双语 + `SECURITY.md` + `SKILL.md`（21KB）+ 发布说明                  |
| 测试     | `node --test`，GitHub Actions 有 CI                                             |
| 端点     | `GET /health`、`/ready`、`/v1/models`、`POST /v1/chat/completions`、本地操作 UI |
| 浏览器   | 用你本机的 Chrome/Edge，Playwright 自带 Chromium 只是兜底                       |
| 特殊能力 | Agent Mode 持久会话、账号池、会话归档、批量测试、可选本地 MCP 隧道              |

**它的定位跟你现在干的活高度重合**：让 Arena 的 Agent Mode 变成一个 OpenAI 兼容接口，本机调用。

**魔改入口**：`src/probe/modules/interceptor.js`（改拦截规则）、`runmodel.js`（431 行，改请求/响应映射）、`registry.js`（442 行，模型注册）。

**要注意的**：

- 早期项目，README 自己写了"依赖 Arena 的未公开运行时行为，随时可能变"
- `exec_command` 是危险面，别开隧道暴露
- 需要 arena.ai 账号

---

## 五、我的建议

三条路，按"省心程度"排：

**A. 直接用它**（最快）

```powershell
git clone https://github.com/Gandhara2077/arena-local-bridge F:\__AI__\work\alb
cd F:\__AI__\work\alb
npm install
npm start          # 127.0.0.1:xxxx
```

装一个 playwright（可以指定用本机 Chrome，不下 Chromium），跑起来就是 OpenAI 兼容接口。手机侧 `arena` 改成普通 HTTP 调用。

**B. 抄它的拦截思路，写进现有 daemon**（最可控）
把 `interceptor.js` 那套"接管 fetch + 旁路 clone"的逻辑，接到现有 `ask_daemon.js` 的 CDP 连接上。我们已经有 9222 常驻 Chrome，只要加一段 `Page.addScriptToEvaluateOnNewDocument` 注入拦截脚本，再用 CDP 的 `Network` 域收数据。大约 150 行，零新增依赖。

**C. 抄 g4f 的 `Arena.py`**（不推荐）
739 行，MIT，2026-10-08 更新。但它硬编码 `create-evaluation`，已经落后一个版本，而且依赖 g4f 整个框架才能跑。

我倾向 **B**：现有 CDP 通道不动，只把"点 DOM 抓文本"换成"拦截页面自己的请求"。这样：

- 端点、token、session 全由页面负责，Arena 改版不影响
- 结束信号从 `ad:` 帧来，不用猜
- 复用已经调通的 ssh + Chrome 拉起链路
- 代码量最小，全部可读可改

要动手的话，先做一件事：在电脑的 Chrome 里打开 arena.ai，用 CDP 挂上 `Network.enable`，跑一轮真实对话，把实际请求和帧格式抓下来验证 `create-chat` 和 `a0:` 的假设。验证过了再改代码。
