//! Windows 11 Snap Layouts 支援
//!
//! 在無邊框（decorations: false）模式下，Windows 11 原生的 Snap Layouts
//! （滑鼠懸停最大化按鈕彈出視窗排版選單）會失效。
//!
//! 本模組透過攔截 `WM_NCHITTEST` 訊息，當滑鼠位於自訂最大化按鈕的對應區域時，
//! 回傳 `HTMAXBUTTON`（0x09），讓 Windows 識別該區域為最大化按鈕，
//! 進而觸發 Snap Layouts 懸停選單。
//!
//! ## 自訂最大化按鈕位置計算
//!
//! 標題列（inject/plugins/titlebar.ts）的按鈕配置：
//! - 標題列高度：32px
//! - 按鈕容器：flex, gap:2px, padding-right:4px
//! - 每個按鈕寬度：34px
//!
//! 從視窗右边缘算起：
//! - Close 按鈕：4px ~ 38px
//! - Max  按鈕：40px ~ 74px  ← Snap Layouts 熱區
//! - Min  按鈕：76px ~ 110px

#[cfg(target_os = "windows")]
mod windows_impl {
    use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
    use windows_sys::Win32::Graphics::Gdi::ScreenToClient;
    use windows_sys::Win32::UI::Shell::{DefSubclassProc, SetWindowSubclass};
    use windows_sys::Win32::UI::WindowsAndMessaging::*;

    /// 子類別 ID（全局唯一即可，本應用只有一個窗口）
    const SUBCLASS_ID: usize = 1;

    /// 標題列高度（px），與 titlebar.ts 的 bar.style.height 一致
    const TITLEBAR_HEIGHT: i32 = 32;

    /// 自訂最大化按鈕的右偏移量（從視窗右邊緣算起）
    /// close_btn(34) + gap(2) + padding(4) = 40px
    const MAX_BTN_RIGHT_OFFSET: i32 = 40;

    /// 自訂最大化按鈕的左偏移量（從視窗右邊緣算起）
    /// MAX_BTN_RIGHT_OFFSET + gap(2) + max_btn(34) = 74px
    const MAX_BTN_LEFT_OFFSET: i32 = 74;

    /// Window Subclass Procedure — 攔截 WM_NCHITTEST
    ///
    /// 回傳值意義：
    /// - `HTMAXBUTTON`：滑鼠在最大化按鈕區域，Windows 會顯示 Snap Layouts
    /// - 其他：交由預設處理（通常回傳 `HTCLIENT`）
    unsafe extern "system" fn subclass_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
        _subclass_id: usize,
        _ref_data: usize,
    ) -> LRESULT {
        if msg == WM_NCHITTEST {
            // 從 LPARAM 提取螢幕座標（lparam 的低 16 位 = x，高 16 位 = y）
            let screen_x = (lparam & 0xFFFF) as i16 as i32;
            let screen_y = ((lparam >> 16) & 0xFFFF) as i16 as i32;

            // 螢幕座標 → 客戶區座標
            let mut point = POINT {
                x: screen_x,
                y: screen_y,
            };
            ScreenToClient(hwnd, &mut point);

            // 取得客戶區大小
            let mut rect = RECT {
                left: 0,
                top: 0,
                right: 0,
                bottom: 0,
            };
            GetClientRect(hwnd, &mut rect);
            let client_width = rect.right - rect.left;

            // 檢查滑鼠是否在自訂最大化按鈕區域
            //
            // 座標判斷（客戶區座標系，(0,0) 在視窗左上角）：
            //   y: 0 ~ TITLEBAR_HEIGHT（標題列範圍）
            //   x: (client_width - MAX_BTN_LEFT_OFFSET) ~ (client_width - MAX_BTN_RIGHT_OFFSET)
            if point.y >= 0 && point.y <= TITLEBAR_HEIGHT {
                let from_right = client_width - point.x;
                if from_right >= MAX_BTN_RIGHT_OFFSET && from_right <= MAX_BTN_LEFT_OFFSET {
                    return HTMAXBUTTON as LRESULT;
                }
            }
        }

        // 非 WM_NCHITTEST 或不在最大化按鈕區域 → 交由預設處理
        DefSubclassProc(hwnd, msg, wparam, lparam)
    }

    /// 安裝 Window Subclass，攔截 WM_NCHITTEST 以支援 Snap Layouts
    ///
    /// 應在 `setup()` 中、視窗建立後呼叫。
    pub fn install(hwnd: HWND) {
        unsafe {
            SetWindowSubclass(
                hwnd,
                Some(subclass_proc),
                SUBCLASS_ID,
                0,
            );
        }
        log::info!("[snap_layout] WM_NCHITTEST subclass 已安裝");
    }
}

/// 安裝 Snap Layouts 支援（僅 Windows 生效）
///
/// 在 `setup()` 中呼叫，傳入 AppHandle。
/// 非 Windows 平台為空操作（no-op）。
pub fn setup(app: &tauri::App) {
    #[cfg(target_os = "windows")]
    {
        use tauri::Manager;
        if let Some(window) = app.get_webview_window("main") {
            // 取得原生視窗句柄 (HWND)
            //
            // Tauri v2 的 WebviewWindow 在 Windows 上提供 .hwnd() 方法，
            // 回傳 Tauri 封裝的 Hwnd 型別。其內部 .0 為原始 HWND 值。
            match window.hwnd() {
                Ok(hwnd) => {
                    // Tauri v2 的 HWND 與 windows-sys 的 HWND 都是 *mut c_void，
                    // 直接傳遞即可。
                    windows_impl::install(hwnd.0);
                }
                Err(e) => {
                    log::error!("[snap_layout] 取得 HWND 失敗: {e}");
                }
            }
        } else {
            log::warn!("[snap_layout] 找不到主視窗，跳過 Snap Layouts 設定");
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = app; // 非 Windows 平台不做任何事
    }
}
