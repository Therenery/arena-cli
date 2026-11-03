//! 把浏览器窗口拉到前台。
//!
//! 桥侧已经用 CDP 的 `Page.bringToFront` 切了标签页；但 Chrome 主窗口
//! 如果最小化或在别的虚拟桌面，光切标签页看不见。所以 Windows 上再补一步：
//! 找标题里带 Chrome / arena 的主窗口，SetForegroundWindow + FlashWindowEx 闪任务栏。
//!
//! 非 Windows 平台这里是空操作（macOS/Linux 上的窗口聚焦交给窗口管理器）。

/// 返回 true 表示确实找到并操作了窗口
pub fn bring_browser_forward() -> bool {
    #[cfg(windows)]
    {
        windows_impl::bring()
    }
    #[cfg(not(windows))]
    {
        false
    }
}

#[cfg(windows)]
mod windows_impl {
    use std::os::raw::c_void;

    type Handle = *mut c_void;

    const SW_RESTORE: i32 = 9;

    // FLASHWINFO
    const FLASHW_ALL: u32 = 0x0000_0003;
    const FLASHW_TIMERNOFG: u32 = 0x0000_000C;

    #[repr(C)]
    struct FlashInfo {
        cb_size: u32,
        hwnd: Handle,
        flags: u32,
        count: u32,
        timeout: u32,
    }

    #[link(name = "user32")]
    extern "system" {
        fn EnumWindows(
            cb: Option<unsafe extern "system" fn(Handle, isize) -> i32>,
            lparam: isize,
        ) -> i32;
        fn GetWindowTextW(hwnd: Handle, buf: *mut u16, max: i32) -> i32;
        fn IsWindowVisible(hwnd: Handle) -> i32;
        fn ShowWindow(hwnd: Handle, cmd: i32) -> i32;
        fn SetForegroundWindow(hwnd: Handle) -> i32;
        fn GetForegroundWindow() -> Handle;
        fn FlashWindowEx(info: *mut FlashInfo) -> i32;
        fn GetWindowThreadProcessId(hwnd: Handle, pid: *mut u32) -> u32;
        fn AttachThreadInput(id_attach: u32, id_attach_to: u32, attach: i32) -> i32;
        fn IsIconic(hwnd: Handle) -> i32;
    }

    struct Search {
        best: Handle,
        best_score: i32,
    }

    const MAX_TITLE: usize = 512;

    unsafe extern "system" fn enum_cb(hwnd: Handle, lparam: isize) -> i32 {
        let state = &mut *(lparam as *mut Search);
        if IsWindowVisible(hwnd) == 0 {
            return 1; // 继续枚举
        }
        let mut buf = [0u16; MAX_TITLE];
        let n = GetWindowTextW(hwnd, buf.as_mut_ptr(), MAX_TITLE as i32);
        if n <= 0 {
            return 1;
        }
        let title = String::from_utf16_lossy(&buf[..n as usize]);
        let lower = title.to_lowercase();
        // 优先 arena 页面，其次任意 Chrome 窗口
        let score = if lower.contains("arena") && lower.contains("chrome") {
            100
        } else if lower.contains("arena") {
            80
        } else if lower.contains("chrome") || lower.contains("chromium") || lower.contains("edge")
        {
            50
        } else {
            0
        };
        if score > 0 && score > state.best_score {
            state.best_score = score;
            state.best = hwnd;
        }
        1
    }

    pub fn bring() -> bool {
        unsafe {
            let mut search = Search {
                best: std::ptr::null_mut(),
                best_score: 0,
            };
            EnumWindows(Some(enum_cb), &mut search as *mut Search as isize);
            if search.best.is_null() {
                return false;
            }
            let hwnd = search.best;
            if IsIconic(hwnd) != 0 {
                ShowWindow(hwnd, SW_RESTORE);
            }
            // 后台进程调 SetForegroundWindow 会被系统拦，先挂到前台线程的输入队列上
            let fg = GetForegroundWindow();
            let target_thread = GetWindowThreadProcessId(hwnd, std::ptr::null_mut());
            let fg_thread = if fg.is_null() {
                0
            } else {
                GetWindowThreadProcessId(fg, std::ptr::null_mut())
            };
            if fg_thread != 0 && target_thread != 0 && fg_thread != target_thread {
                AttachThreadInput(fg_thread, target_thread, 1);
                SetForegroundWindow(hwnd);
                AttachThreadInput(fg_thread, target_thread, 0);
            } else {
                SetForegroundWindow(hwnd);
            }
            // 就算抢不到焦点，任务栏也会闪，用户能注意到
            let mut info = FlashInfo {
                cb_size: std::mem::size_of::<FlashInfo>() as u32,
                hwnd,
                flags: FLASHW_ALL | FLASHW_TIMERNOFG,
                count: 5,
                timeout: 0,
            };
            FlashWindowEx(&mut info);
            true
        }
    }
}
