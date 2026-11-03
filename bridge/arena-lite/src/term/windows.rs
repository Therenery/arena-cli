//! Windows 终端：全部走 Win32 控制台 API，不依赖任何外部命令。
//!
//! - 输出：开 ENABLE_VIRTUAL_TERMINAL_PROCESSING，ANSI 序列直接可用
//! - 输入：关 ENABLE_LINE_INPUT / ENABLE_ECHO_INPUT / ENABLE_PROCESSED_INPUT，
//!   开 ENABLE_VIRTUAL_TERMINAL_INPUT；按键用 ReadConsoleInputW 拿真实键记录，
//!   这样 Shift+Enter 也能和 Enter 区分开
//! - 尺寸：GetConsoleScreenBufferInfo 取可见窗口的行列

use std::os::raw::c_void;
use std::sync::mpsc::Sender;
use std::time::Duration;

use crate::event::Msg;
use crate::keys::Key;

type Handle = *mut c_void;

const STD_INPUT_HANDLE: u32 = 0xFFFF_FFF6; // (DWORD)-10
const STD_OUTPUT_HANDLE: u32 = 0xFFFF_FFF5; // (DWORD)-11

const ENABLE_PROCESSED_INPUT: u32 = 0x0001;
const ENABLE_LINE_INPUT: u32 = 0x0002;
const ENABLE_ECHO_INPUT: u32 = 0x0004;
const ENABLE_QUICK_EDIT_MODE: u32 = 0x0040;
const ENABLE_EXTENDED_FLAGS: u32 = 0x0080;
const ENABLE_VIRTUAL_TERMINAL_INPUT: u32 = 0x0200;

const ENABLE_PROCESSED_OUTPUT: u32 = 0x0001;
const ENABLE_VIRTUAL_TERMINAL_PROCESSING: u32 = 0x0004;

const CP_UTF8: u32 = 65001;

const KEY_EVENT: u32 = 0x0001;

/* ---------- 控制台按键修饰位 ---------- */
const RIGHT_ALT_PRESSED: u32 = 0x0001;
const LEFT_ALT_PRESSED: u32 = 0x0002;
const RIGHT_CTRL_PRESSED: u32 = 0x0004;
const LEFT_CTRL_PRESSED: u32 = 0x0008;
const SHIFT_PRESSED: u32 = 0x0010;

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Coord {
    x: i16,
    y: i16,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct SmallRect {
    left: i16,
    top: i16,
    right: i16,
    bottom: i16,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct ConsoleScreenBufferInfo {
    size: Coord,
    cursor_position: Coord,
    attributes: u16,
    window: SmallRect,
    maximum_window_size: Coord,
}

/// INPUT_RECORD 是 4 字节 EventType + 16 字节 union（KEY_EVENT_RECORD 最大）
#[repr(C)]
struct InputRecord {
    event_type: u32,
    body: [u8; 16],
}

impl InputRecord {
    fn zeroed() -> InputRecord {
        InputRecord {
            event_type: 0,
            body: [0u8; 16],
        }
    }
}

#[link(name = "kernel32")]
extern "system" {
    fn GetStdHandle(which: u32) -> Handle;
    fn GetConsoleMode(handle: Handle, mode: *mut u32) -> i32;
    fn SetConsoleMode(handle: Handle, mode: u32) -> i32;
    fn GetConsoleScreenBufferInfo(handle: Handle, info: *mut ConsoleScreenBufferInfo) -> i32;
    fn ReadConsoleInputW(
        handle: Handle,
        buf: *mut InputRecord,
        len: u32,
        read: *mut u32,
    ) -> i32;
    fn SetConsoleOutputCP(cp: u32) -> i32;
    fn SetConsoleCP(cp: u32) -> i32;
}

/// 启动时调用：切 UTF-8 代码页 + 打开输出端 VT 处理
pub fn init() {
    unsafe {
        SetConsoleOutputCP(CP_UTF8);
        SetConsoleCP(CP_UTF8);
        let hout = GetStdHandle(STD_OUTPUT_HANDLE);
        let mut mode = 0u32;
        if GetConsoleMode(hout, &mut mode) != 0 {
            SetConsoleMode(
                hout,
                mode | ENABLE_PROCESSED_OUTPUT | ENABLE_VIRTUAL_TERMINAL_PROCESSING,
            );
        }
    }
}

pub fn is_tty() -> bool {
    unsafe {
        let mut m = 0u32;
        GetConsoleMode(GetStdHandle(STD_INPUT_HANDLE), &mut m) != 0
            && GetConsoleMode(GetStdHandle(STD_OUTPUT_HANDLE), &mut m) != 0
    }
}

pub fn size() -> crate::term::Size {
    unsafe {
        let h = GetStdHandle(STD_OUTPUT_HANDLE);
        let mut info = ConsoleScreenBufferInfo::default();
        if GetConsoleScreenBufferInfo(h, &mut info) != 0 {
            let cols = info.window.right as i32 - info.window.left as i32 + 1;
            let rows = info.window.bottom as i32 - info.window.top as i32 + 1;
            if cols > 0 && rows > 0 {
                return crate::term::Size {
                    cols: cols.min(500) as u16,
                    rows: rows.min(200) as u16,
                };
            }
        }
    }
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
    in_mode: u32,
    out_mode: u32,
    active: bool,
}

impl Drop for RawMode {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        unsafe {
            SetConsoleMode(GetStdHandle(STD_INPUT_HANDLE), self.in_mode);
            SetConsoleMode(GetStdHandle(STD_OUTPUT_HANDLE), self.out_mode);
        }
        self.active = false;
    }
}

pub fn enter_raw() -> Option<RawMode> {
    unsafe {
        let hin = GetStdHandle(STD_INPUT_HANDLE);
        let hout = GetStdHandle(STD_OUTPUT_HANDLE);
        let mut in_mode = 0u32;
        let mut out_mode = 0u32;
        if GetConsoleMode(hin, &mut in_mode) == 0 {
            return None;
        }
        if GetConsoleMode(hout, &mut out_mode) == 0 {
            return None;
        }
        let want_in = (in_mode | ENABLE_VIRTUAL_TERMINAL_INPUT | ENABLE_EXTENDED_FLAGS)
            & !(ENABLE_LINE_INPUT
                | ENABLE_ECHO_INPUT
                | ENABLE_PROCESSED_INPUT
                | ENABLE_QUICK_EDIT_MODE);
        if SetConsoleMode(hin, want_in) == 0 {
            return None;
        }
        let want_out = out_mode | ENABLE_PROCESSED_OUTPUT | ENABLE_VIRTUAL_TERMINAL_PROCESSING;
        if SetConsoleMode(hout, want_out) == 0 {
            SetConsoleMode(hin, in_mode);
            return None;
        }
        Some(RawMode {
            in_mode,
            out_mode,
            active: true,
        })
    }
}

/// 读键线程：ReadConsoleInputW 阻塞等按键，解出 Key 后发给主循环
pub fn spawn_input(tx: Sender<Msg>) {
    let _ = std::thread::Builder::new()
        .name("arena-keys".to_string())
        .spawn(move || {
            let hin = unsafe { GetStdHandle(STD_INPUT_HANDLE) };
            let mut rec = InputRecord::zeroed();
            loop {
                let mut read: u32 = 0;
                let ok = unsafe { ReadConsoleInputW(hin, &mut rec, 1, &mut read) };
                if ok == 0 || read == 0 {
                    // 控制台暂时不可读（比如切了窗口）：歇一下再来
                    std::thread::sleep(Duration::from_millis(40));
                    continue;
                }
                if rec.event_type != KEY_EVENT {
                    continue; // 窗口尺寸/鼠标事件交给轮询处理
                }
                for k in decode_key(&rec.body) {
                    if tx.send(Msg::Key(k)).is_err() {
                        return;
                    }
                }
            }
        });
}

fn u16_at(b: &[u8; 16], off: usize) -> u16 {
    let mut tmp = [0u8; 2];
    tmp.copy_from_slice(&b[off..off + 2]);
    u16::from_ne_bytes(tmp)
}

fn u32_at(b: &[u8; 16], off: usize) -> u32 {
    let mut tmp = [0u8; 4];
    tmp.copy_from_slice(&b[off..off + 4]);
    u32::from_ne_bytes(tmp)
}

/// KEY_EVENT_RECORD → 一个或多个 Key（长按重复用 repeat count 展开）
fn decode_key(body: &[u8; 16]) -> Vec<Key> {
    let down = u32_at(body, 0);
    if down == 0 {
        return Vec::new(); // 抬起事件
    }
    let repeat = u16_at(body, 4).clamp(1, 32) as usize;
    let vk = u16_at(body, 6);
    let ch = u16_at(body, 10);
    let state = u32_at(body, 12);

    let shift = state & SHIFT_PRESSED != 0;
    let alt = state & (LEFT_ALT_PRESSED | RIGHT_ALT_PRESSED) != 0;
    let ctrl = state & (LEFT_CTRL_PRESSED | RIGHT_CTRL_PRESSED) != 0;

    let key = match vk {
        0x0D => {
            // Shift+Enter / Alt+Enter 都是换行，不发送
            if shift || alt {
                Key::Newline
            } else {
                Key::Enter
            }
        }
        0x08 => Key::Backspace,
        0x09 => {
            if shift {
                Key::BackTab
            } else {
                Key::Tab
            }
        }
        0x1B => Key::Esc,
        0x21 => Key::PageUp,
        0x22 => Key::PageDown,
        0x23 => Key::End,
        0x24 => Key::Home,
        0x25 => Key::Left,
        0x26 => Key::Up,
        0x27 => Key::Right,
        0x28 => Key::Down,
        0x2D => Key::Ignored, // Insert
        0x2E => Key::Delete,
        v @ 0x41..=0x5A if ctrl => Key::Ctrl((b'a' + (v - 0x41) as u8) as char),
        _ => {
            if ch == 0 {
                Key::Ignored
            } else {
                match char::from_u32(ch as u32) {
                    Some(c) if (c as u32) >= 0x20 => Key::Char(c),
                    _ => Key::Ignored,
                }
            }
        }
    };
    vec![key; repeat]
}
