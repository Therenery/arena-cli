# arena_bridge —— 把 Arena.ai 的 Agent 接进本地工作流

拦截式方案：不伪造请求、不碰 reCAPTCHA、不存 cookie。用真键盘事件驱动页面，旁路读取页面自己发出的流。

---

## 一、用起来

### 交互式对话（推荐）

```bash
arena                    # 进 TUI（缺 codex 时自动退到 REPL），Agent 模式，会话 main
arena chat -d            # Direct 模式
arena chat -s 设计       # 独立会话「设计」
```

底层是 aichat 的 REPL，流式输出，多轮上下文由本地客户端保存：

| REPL 命令        | 作用                                   |
| ---------------- | -------------------------------------- |
| `.model`         | 在 arena-agent / arena-direct 之间切换 |
| `.session`       | 切换或新建会话                         |
| `.file`          | 附加文件、目录或 URL                   |
| `.set`           | 调整运行时设置                         |
| `.exit` / Ctrl-D | 退出                                   |
| `:::`            | 多行输入（起止各一行）                 |

### 单发提问

```bash
arena "你的问题"                    # Agent 模式（默认）
arena -d "你的问题"                 # Direct 模式，单模型快速问答
arena -r "你的问题"                 # 附带思考过程
arena -s 名字 "你的问题"            # 指定会话名
arena -n "你的问题"                 # 发问前清空该会话历史
```

单发默认每次都是独立任务。同一会话名连续调用时，服务端会接续上文的记录（上限 24 条），换客户端也不丢。

### 运维

```bash
arena doctor        # 隧道 / 桥接 / Chrome / 模型 / 客户端 体检
arena models        # 可用模型
arena sessions      # 本地保存的会话
arena new [名字]     # 清空会话（服务端历史 + 本地记录）
```

---

## 二、架构

```
手机 / 任意客户端
   │  ssh（pc 命令，三路自适应）
   ▼
电脑 127.0.0.1:17892  arena_bridge.js（node，约 890 行）
   ├─ POST /v1/chat/completions   OpenAI 兼容
   ├─ POST /ask                   简化接口
   ├─ GET  /health  /v1/models  /debug
   │
   ├─ CDP → Chrome 9222（已有，schtasks 任务 chrome-arena 自动拉起）
   │    ├─ Page.addScriptToEvaluateOnNewDocument 注入拦截脚本
   │    └─ 逐字符 dispatchKeyEvent + 真实鼠标点击发送
   │
   └─ 页面自己发请求 → 旁路 clone 读 SSE
        ├─ direct 模式：/nextjs-api/stream/create-evaluation
        │    a0: 正文  ag: 思考  a2: 路由/心跳  ad: 结束
        └─ agent 模式：/ai-proxy/realtime/v1/sessions/{id}/out
             event: batch → records[].body → data.type
             text-delta / reasoning-delta / tool-input-available / finish
```

---

## 三、接进本地工作流

任何支持 OpenAI 协议的客户端，把 base_url 指过来即可：

```
base_url = http://127.0.0.1:17892/v1
api_key  = 任意非空字符串
model    = arena-agent   （或 arena-direct）
```

从手机侧访问，先建隧道。`arena-tunnel` 会按 tsnc → 校园网 v6 → Tailscale 直连三路依次尝试，成功即返回：

```bash
arena-tunnel
curl -s http://127.0.0.1:17892/v1/models
```

手机上的 aichat 已配好这个客户端，直接可用：

```yaml
# /root/.config/aichat/config.yaml
clients:
  - type: openai-compatible
    name: arena
    api_base: http://127.0.0.1:17892/v1
    api_key: sk-arena-local
    models:
      - name: arena-agent
      - name: arena-direct
```

```bash
aichat -m arena:arena-agent                  # REPL
aichat -S -m arena:arena-direct "问题"        # 单发、非流式
```

`arena` 命令就是这套东西的包装：自动建隧道、自动体检、按模式选模型。

请求体可用 `metadata.mode` 覆盖模式，`user` 字段区分会话：

```json
{
  "model": "arena-agent",
  "user": "proj1",
  "messages": [{ "role": "user", "content": "..." }],
  "metadata": { "mode": "agent" }
}
```

响应额外带 `arena.tools` / `arena.reasoning` / `arena.routed`（模型厂商）。

---

## 四、安全设计

| 项        | 做法                                                                |
| --------- | ------------------------------------------------------------------- |
| 凭据      | 不读取、不存储、不外发任何 cookie 或 token                          |
| reCAPTCHA | 完全不碰，由页面自己生成                                            |
| 监听      | 只绑 127.0.0.1，不开公网                                            |
| 并发      | 全局队列串行（同时只跑 1 个），排队上限 4                           |
| 频率      | 两次请求最小间隔 3 秒                                               |
| 输入      | 逐字符 `dispatchKeyEvent` + 真实鼠标点击发送，页面看到的是人        |
| 重试      | 失败不自动重试，避免触发风控                                        |
| 日志      | 默认只记 sid / 游标 / 长度；prompt 正文需 `ARB_DEBUG_PROMPT=1` 才记 |

---

## 五、部署与排障

```powershell
# 服务（计划任务，开机/手动）
schtasks /run /tn arena-bridge

# Chrome（计划任务，session 1）
schtasks /run /tn chrome-arena

# 日志
Get-Content F:\__AI__\tmp\arb.log -Tail 30
```

| 现象                           | 原因           | 处理                                      |
| ------------------------------ | -------------- | ----------------------------------------- |
| `NEED_VERIFY`                  | 页面弹人机验证 | 去电脑 Chrome 点一下                      |
| `chrome unavailable`           | Chrome 没起    | `schtasks /run /tn chrome-arena`，等 5 秒 |
| `page not ready (no composer)` | 页面结构变了   | 更新 `EDITOR_SEL`                         |
| `empty answer from upstream`   | 流结束但无文本 | 多为 review 状态，重试一次                |
| `stream ended with no text`    | 同上           | 同上                                      |
| `busy: queue full`             | 队列满         | 等前一个跑完                              |
| 取回结果失败                   | ssh 传输异常   | 重试；必要时看 `F:\__AI__\tmp\last.json`  |

---

## 六、已知限制

1. **每轮独立**：Agent 模式的会话复用会出现空流（同一 `/out` 流重连，无法区分新旧 record），所以默认每轮新开标签。代价是每轮多约 10 秒页面加载。
2. **慢**：Agent 任务本身 40 秒到 5 分钟，加上页面加载。
3. **依赖 UI**：输入框是 ProseMirror（agent）/ textarea（direct），Arena 改版要跟着改 `EDITOR_SEL`。
4. **标签堆积**：每轮新开一个，建议定期清理（`/json/close/{id}`）。
5. **Agent 会先去翻仓库**：工作区默认挂着上次的仓库，问设计类问题它会先 `ls` 一遍，多花十几秒。
6. **单发非流式**：`arena "问题"` 等全部完成再打印；`arena chat` 走 `/v1/chat/completions` 的 `stream:true`，实时出字。

---

## 七、文件清单（电脑 F:\_\_AI\_\_\work\）

| 文件                    | 作用                              |
| ----------------------- | --------------------------------- |
| `arena_bridge.js`       | 主服务，约 890 行                 |
| `arb.ps1`               | 手机侧调用入口（发请求 + 写结果） |
| `rd2.js`                | 结果回传（gzip + base64）         |
| `start_bridge.ps1`      | 前台启动脚本                      |
| `F:\__AI__\tmp\arb.log` | 运行日志                          |

## 八、手机侧脚本（/usr/local/bin/）

| 文件           | 作用                                                                                                    |
| -------------- | ------------------------------------------------------------------------------------------------------- |
| `arena`        | 总入口：无参进 TUI（缺 codex 时退到 REPL），带问题走单发，另有 `chat/models/sessions/new/doctor` 子命令 |
| `arena-chat`   | REPL 启动器：建隧道 → 体检 → `aichat -m arena:<model> -s <session>`                                     |
| `arena-send`   | 单发实现：base64 → `pc` 调 `arb.ps1` → 取回 gzip 结果 → `jq` 输出                                       |
| `arena-tunnel` | 三路隧道（tsnc → v6 → 直连），成功后把线路写进 `/tmp/.arena-tunnel-route`                               |
| `arena-ask`    | 早期版本，保留兼容                                                                                      |

`/root/.config/aichat/config.yaml` 是 aichat 的客户端配置，改模型或地址只动这一个文件。
