#!/usr/bin/env python3
"""用伪终端驱动 arena-lite 的 TUI，验证真实按键与渲染路径。

不依赖任何第三方包：pty + select 就够了。
"""
import os
import pty
import re
import select
import struct
import sys
import termios
import fcntl
import time

BIN = sys.argv[1] if len(sys.argv) > 1 else "/tmp/arena-check"
BASE = sys.argv[2] if len(sys.argv) > 2 else "127.0.0.1:17899"
COLS, ROWS = 100, 30


def spawn():
    pid, fd = pty.fork()
    if pid == 0:
        os.execv(BIN, [BIN, "--no-logo", "--base", BASE])
    fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", ROWS, COLS, 0, 0))
    return pid, fd


def read_for(fd, seconds, sink):
    end = time.time() + seconds
    while time.time() < end:
        r, _, _ = select.select([fd], [], [], 0.05)
        if r:
            try:
                chunk = os.read(fd, 65536)
            except OSError:
                return
            if not chunk:
                return
            sink.append(chunk.decode("utf-8", "replace"))


def send(fd, data):
    if isinstance(data, str):
        data = data.encode()
    os.write(fd, data)
    time.sleep(0.15)


def strip_ansi(s):
    return re.sub(r"\x1b\[[0-9;?]*[A-Za-z]", "", s)


def main():
    pid, fd = spawn()
    out = []
    read_for(fd, 1.0, out)

    failures = []

    def check(name, cond, extra=""):
        if cond:
            print(f"  ✓ {name}")
        else:
            print(f"  ✗ {name} {extra}")
            failures.append(name)

    screen = strip_ansi("".join(out))
    check("状态栏有模式 agent", "agent" in screen)
    check("状态栏有桥地址", BASE.split(":")[0] in screen)
    check("底部输入提示", "Enter 发送" in screen)

    # 输入并发送
    out.clear()
    send(fd, "你好\r")
    read_for(fd, 3.0, out)
    screen = strip_ansi("".join(out))
    check("流式正文出现", "我是 arena" in screen)
    check("思考链折叠提示", "思考" in screen)
    check("工具块折叠提示", "工具 read_file" in screen)

    # Ctrl+R 展开思考
    out.clear()
    send(fd, "\x12")
    read_for(fd, 0.8, out)
    screen = strip_ansi("".join(out))
    check("Ctrl+R 展开思考", "Ctrl+R 收起" in screen)

    # Ctrl+T 展开工具
    out.clear()
    send(fd, "\x14")
    read_for(fd, 0.8, out)
    screen = strip_ansi("".join(out))
    check("Ctrl+T 展开工具", "path: src/main.rs" in screen)

    # 切 battle 模式
    out.clear()
    send(fd, "/battle\r")
    read_for(fd, 1.0, out)
    screen = strip_ansi("".join(out))
    check("切换到 battle", "battle" in screen)

    # battle 下发一条，检查双栏拼接
    out.clear()
    send(fd, "打一架\r")
    read_for(fd, 3.0, out)
    screen = strip_ansi("".join(out))
    check("battle 双栏答案", "答案甲" in screen and "答案乙" in screen)

    # 上滚：PgUp 后应有「新内容」提示（内容不足以滚动时允许不出现）
    out.clear()
    send(fd, "\x1b[5~")
    read_for(fd, 1.0, out)
    screen = strip_ansi("".join(out))
    print("  · PgUp 后屏幕含「新内容」:", "新内容" in screen)

    # 斜杠命令 /help
    out.clear()
    send(fd, "/help\r")
    read_for(fd, 1.0, out)
    screen = strip_ansi("".join(out))
    check("/help 输出命令列表", "/resume" in screen and "/vote" in screen)

    # Tab 补全
    out.clear()
    send(fd, "/ba\t")
    read_for(fd, 0.5, out)
    screen = strip_ansi("".join(out))
    check("Tab 补全 /battle", "/battle" in screen)

    # Esc 清空
    send(fd, "\x1b")
    out.clear()
    read_for(fd, 0.5, out)

    # /resume 载入网页端历史
    out.clear()
    send(fd, "/resume main\r")
    read_for(fd, 1.5, out)
    screen = strip_ansi("".join(out))
    check("/resume 载入网页端历史", "这是网页端的第一句" in screen)
    check("/resume 有载入提示", "已载入会话" in screen)

    # 流式中断：SLOW 会持续吐 6 秒
    out.clear()
    send(fd, "SLOW\r")
    read_for(fd, 1.2, out)
    screen = strip_ansi("".join(out))
    check("慢速流式有输出", "第1段" in screen)
    send(fd, "\x03")
    out.clear()
    read_for(fd, 1.0, out)
    screen = strip_ansi("".join(out))
    check("Ctrl+C 中断生成", "已中断本次生成" in screen)
    try:
        wpid, _status = os.waitpid(pid, os.WNOHANG)
        alive = wpid != pid
    except ChildProcessError:
        alive = False
    check("中断后进程还活着", alive)

    # Ctrl+C 退出（空闲态）
    send(fd, "\x03")
    read_for(fd, 1.0, out)
    try:
        wpid, status = os.waitpid(pid, os.WNOHANG)
        exited = wpid == pid
    except ChildProcessError:
        exited = True
    check("Ctrl+C 退出", exited)
    if not exited:
        os.kill(pid, 9)
        os.waitpid(pid, 0)

    print()
    if failures:
        print("失败项：", "、".join(failures))
        return 1
    print("TUI 全部检查通过")
    return 0


if __name__ == "__main__":
    sys.exit(main())
