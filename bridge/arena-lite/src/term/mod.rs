//! 终端层入口：按平台分派。
//!
//! Windows 走 Win32 控制台 API（SetConsoleMode / GetConsoleScreenBufferInfo /
//! ReadConsoleInputW），Unix 走 termios + ioctl + poll。
//! **任何平台分支里都不调用外部命令**（上一版用 `stty` 就是在这儿翻的车）。

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Size {
    pub cols: u16,
    pub rows: u16,
}

impl Size {
    pub fn sane(self) -> Size {
        Size {
            cols: self.cols.clamp(20, 500),
            rows: self.rows.clamp(6, 200),
        }
    }
}

#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub use unix::{enter_raw, spawn_input};

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::{enter_raw, spawn_input};

/// 进程启动时做一次平台初始化（Windows 上开 VT、切 UTF-8 代码页）
pub fn init() {
    #[cfg(windows)]
    windows::init();
}

/// 终端尺寸
pub fn size() -> Size {
    #[cfg(unix)]
    {
        unix::size()
    }
    #[cfg(windows)]
    {
        windows::size()
    }
    #[cfg(not(any(unix, windows)))]
    {
        Size {
            cols: 80,
            rows: 24,
        }
    }
}

/// stdin/stdout 是不是一个真终端
pub fn is_tty() -> bool {
    #[cfg(unix)]
    {
        unix::is_tty()
    }
    #[cfg(windows)]
    {
        windows::is_tty()
    }
    #[cfg(not(any(unix, windows)))]
    {
        false
    }
}
