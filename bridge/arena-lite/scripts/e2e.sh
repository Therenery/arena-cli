#!/usr/bin/env bash
# 端到端冒烟：起一个假桥（node，无需 Chrome），用真二进制跑一遍。
#
#   bash scripts/e2e.sh /path/to/arena-lite
#
# 依赖：node >= 18（假桥）、python3（伪终端驱动 TUI）。都没有的话会自动跳过对应部分。
set -uo pipefail

cd "$(dirname "$0")/.."
BIN="${1:-build/arena-lite}"
PORT="${PORT:-17899}"
BASE="127.0.0.1:$PORT"
FAIL=0

pass() { echo "  ✓ $1"; }
fail() { echo "  ✗ $1"; FAIL=$((FAIL + 1)); }

if [ ! -x "$BIN" ]; then
  echo "找不到二进制：$BIN（先跑 scripts/check.sh 或 cargo build --release）"
  exit 2
fi

command -v node >/dev/null || {
  echo "没有 node，跳过 e2e"
  exit 0
}

node scripts/mock_bridge.js >/tmp/arena-lite-mock.log 2>&1 &
MOCK_PID=$!
trap 'kill $MOCK_PID $MOCK2_PID 2>/dev/null' EXIT
sleep 1

echo "== 单发"
OUT=$("$BIN" --base "$BASE" "你好" 2>/dev/null)
case "$OUT" in
  *"我是 arena"*) pass "流式正文" ;;
  *) fail "流式正文（拿到：$OUT）" ;;
esac

echo "== 单发 + 思考链"
ERR=$("$BIN" --base "$BASE" --reasoning "你好" 2>&1 >/dev/null)
case "$ERR" in
  *"工具 read_file"*) pass "工具行拆分成工具块" ;;
  *) fail "工具行拆分（stderr：$ERR）" ;;
esac

echo "== 模式参数"
OUT=$("$BIN" --base "$BASE" --mode battle "打一架" 2>/dev/null)
case "$OUT" in
  *"答案甲"*"答案乙"*) pass "battle 双栏答案" ;;
  *) fail "battle 双栏（拿到：$OUT）" ;;
esac

echo "== health"
"$BIN" --base "$BASE" health >/dev/null 2>&1 && pass "health" || fail "health"

echo "== 队列满的人话错误"
MSG=$("$BIN" --base "$BASE" "QUEUE" 2>&1 >/dev/null)
case "$MSG" in
  *"队列满"*) pass "429 翻译成人话" ;;
  *) fail "429 翻译（拿到：$MSG）" ;;
esac

echo "== 桥没起来的人话错误"
MSG=$("$BIN" --base 127.0.0.1:1 "你好" 2>&1 >/dev/null)
case "$MSG" in
  *"连不上桥"*) pass "连不上桥的人话错误" ;;
  *) fail "连不上桥（拿到：$MSG）" ;;
esac

echo "== history"
OUT=$("$BIN" --base "$BASE" history main 2>/dev/null)
case "$OUT" in
  *"网页端的第一句"*) pass "history 打印网页端历史" ;;
  *) fail "history（拿到：$OUT）" ;;
esac

echo "== resume（读不到要明确报错、退出码非 0）"
"$BIN" --base "$BASE" resume nope "接着聊" >/dev/null 2>&1
[ $? -ne 0 ] && pass "读不到历史时非 0 退出" || fail "读不到历史时应当非 0 退出"

MOCK2_PID=${MOCK2_PID:-0}

if command -v python3 >/dev/null; then
  echo "== 409 验证 → 自动重试一次（起第二个假桥，第一次必 409）"
PORT2=$((PORT + 1))
VERIFY_TIMES=1 PORT=$PORT2 node scripts/mock_bridge.js >/tmp/arena-lite-mock2.log 2>&1 &
MOCK2_PID=$!
sleep 1
RETRY_OUT=$("$BIN" --base "127.0.0.1:$PORT2" "VERIFY 一下" 2>/tmp/arena-lite-retry.err)
RETRY_ERR=$(cat /tmp/arena-lite-retry.err)
kill $MOCK2_PID 2>/dev/null
case "$RETRY_ERR" in
  *"需要人工验证"*) pass "409 时提示已切浏览器" ;;
  *) fail "409 提示（stderr：$RETRY_ERR）" ;;
esac
case "$RETRY_OUT" in
  *"我是 arena"*) pass "验证后自动重试成功" ;;
  *) fail "自动重试（拿到：$RETRY_OUT）" ;;
esac

echo "== TUI（伪终端）"
  if python3 scripts/tui_smoke.py "$BIN" "$BASE"; then
    pass "TUI 冒烟"
  else
    fail "TUI 冒烟"
  fi
else
  echo "  · 没有 python3，跳过 TUI 冒烟"
fi

echo
if [ "$FAIL" -eq 0 ]; then
  echo "e2e 全部通过"
else
  echo "e2e 有 $FAIL 项失败"
fi
exit "$FAIL"
