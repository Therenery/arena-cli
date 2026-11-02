# 代码评审（第二轮）· 2026-10-31

范围：`arena_bridge.js`（965 → 1013 行）、新增 `lib/parse.js` + `test/`、`cli/` 五个脚本、`arb.ps1`、文档。
上一轮（`docs/review-2026-10-31.md`）的结论是「架构选得对，问题集中在失败时不吭声」。本轮按这条线索又跑了一遍真机路径，找到 4 个会让链路**看起来只是卡住**的 bug，其中 1 个是崩溃级别。

结论先行：**协议层（帧解析）现在有测试护住了；剩下的坑主要在「HTTP 层出错之后收没收尾」和「CLI 的默认体验」。**

---

## 一、本轮修的 bug

### P0-1 SSE 分支 catch 里引用了未定义的变量，流永远不结束

```js
} catch (e) {
  log('v1 error:', e.message);
  if (s) { ... }          // s 只存在于 run() 内部，这里从未声明
```

`/v1/chat/completions` 走 stream 时，只要 `askOnce` 抛错（Chrome 没起来、超时、风控、上游错误都算），catch 里第一句就 `ReferenceError: s is not defined`：

- 异常从 catch 里再次抛出 → `endSSE()` 和 `return` 都被跳过
- 响应头早就发过了，客户端只能一直收 `: ping`
- `resetSession` / `stopGeneration` 一次都没执行过（它们被 `if (s)` 包着）

沙箱里实测（Chrome 未启动，60 秒后超时）：

```
14:07:03 v1 error: chrome unavailable
14:07:03 unhandled ReferenceError: s is not defined
   at Server.<anonymous> (arena_bridge.js:862:9)
```

客户端那侧看到的是连接挂着、心跳不断、没有任何回答——恰恰是最难排查的形态。

**修法**：把本轮实际使用的 session 提到 handler 作用域（`let live`），`endSSE()` 放进 `finally`，并保证错误帧后补一条 `data: [DONE]`。修复后同一场景：

```
data: {"error":{"message":"chrome unavailable","type":"upstream_error"}}
data: [DONE]
```

连接在 60 秒时正常关闭，日志里不再有 ReferenceError。

### P0-2 非流式接口无条件 `resetSession`，队列满时会杀掉正在跑的会话

```js
} catch (e) {
  try { await resetSession(name); } catch (e9) {}   // 任何错误都杀
```

三种代价被算错了：

1. `busy: queue full` 不是会话的问题，却会关掉这个会话的标签页——**把它正在跑的那一轮连根拔掉**
2. 瞬时错误（`no stream appeared`）重开后要重新加载页面，约 10 秒
3. 状态码也没分：队列满返回 500，客户端无法区分「该重试」还是「真坏了」

**修法**：`sessionBroken()` 只对结构性错误（composer 还在、tab 关了、ws 断了、NEED…）重置；队列满一律 429，客户端断开 499。`/ask` 补上了同样的处理（之前它连重置都没有）。

### P0-3 每条新会话的第一问都白等 25 秒

`askOnce` 在输入前调 `waitConversation()`，它等 URL 变成 `/agent/<uuid>`。但**新标签页停在裸 `/agent` 时是没有 uuid 的**，于是一路轮询到 25000ms 上限才返回 false（返回值还被忽略）。

**修法**：URL 就是裸 `/agent` 或 `/text/direct` 时直接返回——本来就没有东西可等；只有在 URL 处于其它中间态时才轮询，并把超时记进日志。

### P0-4 帧号坐标系不一致，长流会重复输出正文

`listStreams()` 返回 `n: s.frames.length`（相对），`readFrames()` 返回 `n: base + s.frames.length`（绝对）。注入脚本在帧数超 3 万时会裁剪并累加 `base`，两个坐标系一旦分叉，游标就比实际小，**已读过的帧会再读一遍**，正文重复。

**修法**：`listStreams()` 也返回绝对帧号。

### 其余修复

| #   | 问题                                                                                                                | 位置                               | 修法                                                                              |
| --- | ------------------------------------------------------------------------------------------------------------------- | ---------------------------------- | --------------------------------------------------------------------------------- |
| 5   | prompt 清洗正则 `/<\\/?[a-z-]+-[a-z-]+>/` 会吃掉用户内容——问「`<my-component>` 为什么会这样」时标签连同名字一起消失 | `/v1/chat/completions`             | 只剥客户端确实会注入的固定标签（`system-reminder`、`environment_details` 等）     |
| 6   | 标签页被重置 / LRU 淘汰后，页面没上下文但桥还认为有 → 对话中途**静默失忆**                                          | `resetSession` / `getSession`      | `contextLost` 标记，下一条请求自动把服务端历史补进 prompt，成功后清除             |
| 7   | `/repo`、`/upload` 不走队列，可以在回答生成到一半时去点页面/塞文件                                                  | 两个 handler                       | 统一走 `enqueue()`；忙时返回 429                                                  |
| 8   | 没有 CORS，也没有任何鉴权（隧道把端口曝到手机上时同机任意进程可用）                                                 | 全局                               | `Access-Control-*` + `OPTIONS 204`；`ARB_TOKEN` 非空时校验 Bearer，`/health` 放行 |
| 9   | `listen` 失败（EADDRINUSE）只记一行日志，进程活着但不监听                                                           | 启动段                             | `server.on('error')` 直接退出并说明原因                                           |
| 10  | `histories` 的键由客户端 `user` 字段决定，**无上限**                                                                | 全局                               | `ARB_MAX_HISTORY_KEYS`（默认 32）按键 LRU                                         |
| 11  | `s.busy` 是死字段，淘汰和退出都读不到真实状态                                                                       | `run()`                            | 真的置位/复位，退出时先停生成再关标签                                             |
| 12  | `/repo` 成功后不清服务端历史，切库后第一问会带着旧库的上下文                                                        | `/repo`                            | 只有真的切了（`changed`）才清；无操作时不误清                                     |
| 13  | `/upload` 的 `objectGroup` 从不释放                                                                                 | `/upload`                          | `Runtime.releaseObjectGroup`                                                      |
| 14  | 会话名默认值不一致：`/ask` 用 `default`，`/v1` 和 `/new` 用 `main`                                                  | 三处                               | 统一 `main`（`DEFAULT_SESSION`）                                                  |
| 15  | 队列满时非流式返回 500，流式返回 429                                                                                | 两处                               | 统一 429；`/health` 的 `queue` 明确是排队数（不含正在跑的）                       |
| 16  | `arb.ps1` 手拼 JSON 转义不全（反斜杠 / 换行会让整段变坏 JSON，客户端只看到「传输异常」）                            | `arb.ps1`                          | 交给 `ConvertTo-Json`                                                             |
| 17  | `arena-send -s`（少给参数）报 `$2: unbound variable` / `shift` 越界                                                 | `cli/arena-send`、`cli/arena-chat` | `need_val()` 明确报「缺少参数：-s」                                               |
| 18  | 裸 `arena` 走 `arena-codex`，但适配器是空文件 → 一上来就报错；README 却写「进 REPL」                                | `cli/arena`                        | 缺 codex 时自动退到 `arena chat` 并说明原因；文档改成实际行为                     |

### 顺带的重构：帧解析抽成纯函数 + 测试

`parseFrame()` 从 `askOnce` 里搬进 `lib/parse.js`，19 个用例覆盖 `a0/ag/a2/a3/ad`、`records[]` 里的四种 `data.type`、坏 JSON、心跳、未知行。以后 Arena 改帧格式，是「改一个函数 + `npm test`」，不用再连真机猜。

---

## 二、还没动，建议排期

| 优先级 | 事项                             | 说明                                                                                                                                                                      |
| ------ | -------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| P0     | `cli/arena-responses` 仍是空文件 | 文档四处指向它，实际链路跑不起来。要么补（约 150 行，Responses→chat/completions），要么从文档里彻底删掉                                                                   |
| P1     | `tools` → `tool_calls`           | 页面已经给了 `tool-input-available`（`toolName` + `input`），现在只当装饰文字打在思考区。映射成 OpenAI 的 `tool_calls` delta，就从「聊天接口」变成「能给 agent 用的接口」 |
| P1     | 输入长度校验                     | 逐字符 `dispatchKeyEvent` 后只校验「非空」，不校验长度。丢字符不会发现。按 `VAL_JS` 与 `body.length` 比对（忽略空白），不等就补差量                                       |
| P1     | `arb.ps1` 的命令行长度           | base64 塞 argv，中文约 1700 字撞 Windows 上限。目前只是**提前报错**，真正修法是 stdin 或临时文件                                                                          |
| P2     | `usage` 用字符数冒充 token       | 客户端算上下文占用全错                                                                                                                                                    |
| P2     | 逐字符输入慢                     | 6000 字 = 6000 次 CDP 往返，光敲字 6~20 秒；可选「按词/按行批量 + `Input.insertText` 后补真实键盘事件」的混合策略，需要真机验证 tiptap 是否买账                           |
| P2     | `/health` 有副作用               | Chrome 没起来时，每次 health 都会触发一次拉起流程（虽有 `chromeLaunching` 兜底）。监控轮询期间会让机器反复试启动                                                          |
| P2     | SSE 出错时无法回 5xx             | 头已发出，只能推 error 帧。若要真状态码，得改成「等首帧再发头」，代价是失去心跳语义——不建议改                                                                             |

---

## 三、交互建议

1. **等待过程要有阶段感。** `arena "问题"` 现在只有一行「已等 N 秒」+ 轮转数字，最长要等 5 分钟。桥这边其实知道自己在干什么，建议 `/health` 增加 `stage`（`queueing / opening-tab / waiting-ready / typing / generating / review-state`）和 `current`（会话名、已用时间），`arena doctor` 直接读出来。这样「卡住了」和「在跑工具」能一眼分开。
2. **冷启动要解释。** Chrome 没起来时固定等 60 秒（40 × 1.5s），CLI 侧完全没提示。建议 `/health` 返回 `chromeStarting: true`，CLI 打一行「正在拉起 Chrome，最多 60 秒」。
3. **队列语义要在返回里说清楚。** 排队上限 4、并发恒为 1，客户端以为能并发 4 个。建议 429 时带 `Retry-After`（按当前一轮的预估剩余时间，至少 30s），aichat/Qwen Code 这类客户端会自己退避。
4. **错误文案分三层。** 现在 CLI 只把 `NEED_VERIFY`、`busy`、`chrome` 三类翻译成中文，其它原样打印。建议把 `sessionBroken` 那一组（composer 卡住 / 标签页关了）也翻译成「页面状态异常，已重置会话，重发即可」，否则用户会以为是内容问题。
5. **`arena new` 默认清当前会话而不是固定 `main`。** `arena -s 设计 "..."` 之后要清 `设计`，还得记得加参数。
6. **REPL/TUI 里显示思考片段而不是空白。** 桥已经在推 `reasoning_content` 与工具调用行（`· bash {...}`），aichat 会显示在思考区；但 TUI 那条链路依赖那个不存在的适配器。打通后这些信息是「长任务不像卡死」的关键。
7. **Ctrl-C 现在真的会停。** 客户端断开后这一轮立刻中止（不再跑满 10 分钟占队列），文档和 `arena --help` 里值得写一句。
8. **`ARB_TOKEN` 的默认值选择。** 现在默认不校验（向后兼容）。如果这台机器长期开着隧道，建议在 README 的快速开始里直接给出「开隧道就设 `ARB_TOKEN`」的一行命令，别只在配置表里躺一行。
9. **`node ≥ 21` 的失败要好看。** 低于 21 时会在第一个请求上炸 `WebSocket is not defined`。建议启动时检查 `typeof WebSocket` 并打印「需要 node ≥ 21（当前 vXX）」。

---

## 四、本轮验证

已验证（沙箱内，无 Windows / Chrome）：

- `npm test`：19 passed
- `node --check arena_bridge.js`、`bash -n` 全部 CLI 脚本
- 启动冒烟：`/health` 返回 `version/uptime/limits/auth`；`/v1/models`、404、坏 JSON 正常
- 鉴权：无 token → 401，带 token → 200，`/health` 放行；`OPTIONS` → 204 + CORS 头
- `/upload` 白名单外 → 403
- **修复前后对比**：stream 请求在 Chrome 缺失时，修复前 SSE 永不结束（只有 ping），修复后收到 error 帧 + `[DONE]` 并正常关闭；日志无 ReferenceError
- 客户端断开：`curl -m 5` 中断后日志出现 `client gone, stopping generation`
- `/ask` 非流式在 Chrome 缺失时返回 500 + `chrome unavailable`，不再误杀会话

**未验证**（需要真机）：CDP 逐字符输入、`waitConversation` 改动后的首问延迟、`/repo` 与 `/upload` 走队列后的实际交互、review 状态恢复。请在电脑上跑一次：

```
arena "换个说法介绍一下这个仓库"
arena -n "再说一遍"
```

确认没有回归，并观察首问是否比之前快（预期省掉 25 秒）。
