//! Unix 终端：termios + ioctl(TIOCGWINSZ) + poll，全部自己声明 FFI。
//!
//! 注意：这里**不用** libc crate（零依赖），也不调用 `stty`。

#![allow(non_camel_case_types)]

use std::os::raw::{c_int, c_ulong};
use std::sync::mpsc::Sender;

use crate::event::Msg;
use crate::keys::KeyParser;

const STDIN_FD: c_int = 0;
const STDOUT_FD: c_int = 1;
const TCSANOW: c_int = 0;
const POLLIN: i16 = 1;

#[repr(C)]
struct Winsize {
    ws_row: u16,
    ws_col: u16,
    ws_xpixel: u16,
    ws_ypixel: u16,
}

#[repr(C)]
struct PollFd {
    fd: c_int,
    events: i16,
    revents: i16,
}

extern "C" {
    fn isatty(fd: c_int) -> c_int;
    fn tcgetattr(fd: c_int, termios_p: *mut u8) -> c_int;
    fn tcsetattr(fd: c_int, optional_actions: c_int, termios_p: *const u8) -> c_int;
    fn ioctl(fd: c_int, request: c_ulong, ...) -> c_int;
    fn poll(fds: *mut PollFd, nfds: c_ulong, timeout: c_int) -> c_int;
    fn read(fd: c_int, buf: *mut u8, count: usize) -> isize;
}

/* ---------- 平台常量（termios 的布局在 Linux 和 BSD 上不一样） ---------- */

#[cfg(any(target_os = "linux", target_os = "android"))]
mod consts {
    use super::c_ulong;
    pub const TIOCGWINSZ: c_ulong = 0x5413;
    pub const OFF_LFLAG: usize = 12;
    pub const OFF_CC: usize = 17;
    pub const IDX_VMIN: usize = 6;
    pub const IDX_VTIME: usize = 5;
    pub const ISIG: u32 = 0x0001;
    pub const ICANON: u32 = 0x0002;
    pub const ECHO: u32 = 0x0008;
    pub const IEXTEN: u32 = 0x8000;
    pub const ICRNL: u32 = 0x0100;
    pub const IXON: u32 = 0x0400;
    pub const OFF_IFLAG: usize = 0;
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
mod consts {
    use super::c_ulong;
    pub const TIOCGWINSZ: c_ulong = 0x40087468;
    pub const OFF_LFLAG: usize = 12;
    pub const OFF_CC: usize = 16;
    pub const IDX_VMIN: usize = 16;
    pub const IDX_VTIME: usize = 17;
    pub const ISIG: u32 = 0x0080;
    pub const ICANON: u32 = 0x0100;
    pub const ECHO: u32 = 0x0008;
    pub const IEXTEN: u32 = 0x0400;
    pub const ICRNL: u32 = 0x0100;
    pub const IXON: u32 = 0x0200;
    pub const OFF_IFLAG: usize = 0;
}

#[cfg(not(any(
    target_os = "linux",
    target_os = "android",
    target_os = "macos",
    target_os = "ios"
)))]
mod consts {
    use super::c_ulong;
    // FreeBSD / 其它 BSD：布局与 macOS 一致
    pub const TIOCGWINSZ: c_ulong = 0x40087468;
    pub const OFF_LFLAG: usize = 12;
    pub const OFF_CC: usize = 16;
    pub const IDX_VMIN: usize = 16;
    pub const IDX_VTIME: usize = 17;
    pub const ISIG: u32 = 0x0080;
    pub const ICANON: u32 = 0x0100;
    pub const ECHO: u32 = 0x0008;
    pub const IEXTEN: u32 = 0x0400;
    pub const ICRNL: u32 = 0x0100;
    pub const IXON: u32 = 0x0200;
    pub const OFF_IFLAG: usize = 0;
}

/// 256 字节足够放下任何平台的 struct termios（Linux 60 / macOS 72）
#[repr(align(8))]
#[derive(Clone, Copy)]
struct TermiosBuf([u8; 256]);

impl TermiosBuf {
    fn new() -> Self {
        TermiosBuf([0u8; 256])
    }

    fn get_u32(&self, off: usize) -> u32 {
        let mut b = [0u8; 4];
        b.copy_from_slice(&self.0[off..off + 4]);
        u32::from_ne_bytes(b)
    }

    fn set_u32(&mut self, off: usize, v: u32) {
        self.0[off..off + 4].copy_from_slice(&v.to_ne_bytes());
    }
}

pub fn is_tty() -> bool {
    unsafe { isatty(STDIN_FD) == 1 && isatty(STDOUT_FD) == 1 }
}

pub fn size() -> crate::term::Size {
    let mut ws = Winsize {
        ws_row: 0,
        ws_col: 0,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    let ok = unsafe { ioctl(STDOUT_FD, consts::TIOCGWINSZ, &mut ws as *mut Winsize) } == 0;
    if ok && ws.ws_col > 0 && ws.ws_row > 0 {
        return crate::term::Size {
            cols: ws.ws_col,
            rows: ws.ws_row,
        };
    }
    // 退化：环境变量 → 80x24
    let cols = std::env::var("COLUMNS")
        .ok()
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or(80);
    let rows = std::env::var("LINES")
        .ok()
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or(24);
    crate::term::Size { cols, rows }
}

pub struct RawMode {
    orig: TermiosBuf,
    active: bool,
}

impl Drop for RawMode {
    fn drop(&mut self) {
        if self.active {
            unsafe {
                tcsetattr(STDIN_FD, TCSANOW, self.orig.0.as_ptr());
            }
            self.active = false;
        }
    }
}

pub fn enter_raw() -> Option<RawMode> {
    if !is_tty() {
        return None;
    }
    let mut buf = TermiosBuf::new();
    let rc = unsafe { tcgetattr(STDIN_FD, buf.0.as_mut_ptr()) };
    if rc != 0 {
        return None;
    }
    let orig = buf;

    // 关掉行缓冲、回显、信号、扩展处理：Ctrl+C 要作为按键拿回来自己处理
    let lflag = buf.get_u32(consts::OFF_LFLAG);
    let lflag = lflag & !(consts::ISIG | consts::ICANON | consts::ECHO | consts::IEXTEN);
    buf.set_u32(consts::OFF_LFLAG, lflag);
    // 关掉 ICRNL：否则 Enter 会被转成 \n，和 Ctrl+J 分不开
    let iflag = buf.get_u32(consts::OFF_IFLAG);
    buf.set_u32(consts::OFF_IFLAG, iflag & !(consts::ICRNL | consts::IXON));
    // 阻塞读，最少 1 字节
    buf.0[consts::OFF_CC + consts::IDX_VMIN] = 1;
    buf.0[consts::OFF_CC + consts::IDX_VTIME] = 0;

    let rc = unsafe { tcsetattr(STDIN_FD, TCSANOW, buf.0.as_ptr()) };
    if rc != 0 {
        return None;
    }
    Some(RawMode {
        orig,
        active: true,
    })
}

/// 起一条读键线程：poll 100ms，超时就把孤立的 ESC 判成 Esc
pub fn spawn_input(tx: Sender<Msg>) {
    let _ = std::thread::Builder::new()
        .name("arena-keys".to_string())
        .spawn(move || {
            let mut parser = KeyParser::new();
            let mut buf = [0u8; 1024];
            loop {
                let mut fds = [PollFd {
                    fd: STDIN_FD,
                    events: POLLIN,
                    revents: 0,
                }];
                let r = unsafe { poll(fds.as_mut_ptr(), 1, 100) };
                let mut keys = Vec::new();
                if r > 0 {
                    let n = unsafe { read(STDIN_FD, buf.as_mut_ptr(), buf.len()) };
                    if n > 0 {
                        parser.push(&buf[..n as usize], &mut keys);
                    } else if n == 0 {
                        // stdin 关了，读键线程到此为止
                        return;
                    }
                } else {
                    parser.timeout(&mut keys);
                }
                for k in keys {
                    if tx.send(Msg::Key(k)).is_err() {
                        return;
                    }
                }
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn termios_buffer_offsets_are_stable() {
        // 只是守卫：如果常量写错了，这里会明显跑偏
        assert_eq!(consts::OFF_LFLAG, 12);
        assert!(consts::OFF_CC >= 16);
        let mut b = TermiosBuf::new();
        b.set_u32(consts::OFF_LFLAG, 0xdead_beef);
        assert_eq!(b.get_u32(consts::OFF_LFLAG), 0xdead_beef);
        assert_eq!(b.0.len(), 256);
    }
}
