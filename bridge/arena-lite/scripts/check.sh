#!/usr/bin/env bash
# 没有 cargo 时的等价构建脚本：直接用 rustc 编。
#
# 这个 crate 零第三方依赖，所以 cargo 唯一多做的事就是解析依赖图 +
# 传一堆 --extern；这里用 rustc 一把过，结果等价（Windows 上请照常
# 用 `cargo build --release`，能拿到单个 arena-lite.exe）。
#
# 用法：
#   scripts/check.sh            # 编 release + 跑单测
#   scripts/check.sh build      # 产物目录
set -euo pipefail

cd "$(dirname "$0")/.."
OUT="${1:-build}"
export PATH="${RUSTC_DIR:-}:$PATH"

rustc --version
mkdir -p "$OUT"

echo "==> 编译 release"
rustc --edition 2021 -O -C panic=abort -o "$OUT/arena-lite" src/main.rs

echo "==> 编译测试"
rustc --edition 2021 --test -o "$OUT/arena-lite-tests" src/main.rs

echo "==> 跑单元测试"
"$OUT/arena-lite-tests"

echo "==> 版本自检"
"$OUT/arena-lite" --version

# 装了 Windows 目标的话顺手做一次跨平台类型检查（零警告才算过）
if rustc --print target-list | grep -qx 'x86_64-pc-windows-gnu'; then
  if rustc --target x86_64-pc-windows-gnu --print sysroot >/dev/null 2>&1; then
    echo "==> Windows 目标类型检查（x86_64-pc-windows-gnu）"
    rustc --edition 2021 --crate-name arena_lite --target x86_64-pc-windows-gnu \
      --emit=obj -C opt-level=2 -o "$OUT/arena-lite-win.o" src/main.rs
    echo "    windows 目标编译通过：$OUT/arena-lite-win.o"
  fi
fi

echo "全部完成。产物在 $OUT/"
