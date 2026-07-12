//! Proxy 守護程序模組 — 管理 Go proxy sidecar 的生命週期
//!
//! 完整移植自 `src/main/common/proxyDaemon.ts`，包含：
//! - 進程啟動 / 監控 / 重啟 / 關閉
//! - 心跳檢測（定期驗證進程存活）
//! - 異常退出後自動重啟（可配置最大次數與重置時間）
//! - 防孤兒進程機制（共享子進程句柄，退出時同步強制 kill）
//!
//! ## 架構重點（第二輪重構）
//!
//! 為了徹底防止「主進程退出但 Go sidecar 仍在運行」的孤兒進程問題，
//! 採用「共享子進程句柄」設計：
//!
//! 1. `child_cell: Arc<std::sync::Mutex<Option<CommandChild>>>` 是唯一的子進程持有者，
//!    同時被 `ProxyDaemon`（守護邏輯）與 `tauri::State`（退出處理）共享。
//! 2. 使用 **`std::sync::Mutex`** 而非 tokio Mutex —— 因為 `CommandChild::kill()` 是同步
//!    且極短的操作，且退出處理發生在主執行緒，此時 tokio runtime 可能已在拆除中，
//!    同步鎖確保 `kill()` 不依賴 runtime 仍存活。
//! 3. `lib.rs::setup()` 在啟動 sidecar **之前**就 `manage()` 這個 cell，
//!    消除「sidecar 已啟動但 state 尚未註冊」的競態。

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::AppHandle;
use tauri_plugin_shell::process::{CommandChild, CommandEvent, TerminatedPayload};
use tauri_plugin_shell::ShellExt;
use tokio::sync::Mutex as AsyncMutex;
use tokio::time::interval;

// ─── 共享子進程句柄 ─────────────────────────────────────────────────

/// 子進程句柄的共享容器。
///
/// 這是防孤兒機制的核心：無論守護邏輯處於何種狀態（重啟中、心跳中），
/// 退出處理都能拿到當前的子進程並同步 kill。
///
/// 使用 `std::sync::Mutex` 而非 `tokio::sync::Mutex`，原因：
/// - `CommandChild::kill()` 是同步呼叫，持鎖時間極短（毫秒級）
/// - 退出處理在主執行緒執行，不能依賴 tokio runtime（可能已拆除）
pub type ChildCell = Arc<Mutex<Option<CommandChild>>>;

/// 建立一個空的共享子進程容器（供 `lib.rs::setup()` 預先 manage）
pub fn new_child_cell() -> ChildCell {
    Arc::new(Mutex::new(None))
}

// ─── 配置 ───────────────────────────────────────────────────────────

/// 守護程序配置（對應 TypeScript `ProxyDaemonConfig`）
#[derive(Clone, Debug)]
pub struct ProxyDaemonConfig {
    /// 重啟延遲（預設 3 秒）
    pub restart_delay: Duration,
    /// 最大重啟次數（預設 5 次）
    pub max_restart_attempts: u32,
    /// 重啟計數重置時間（預設 60 秒）
    pub restart_attempt_reset_time: Duration,
    /// 是否啟用心跳檢測（預設 true）
    pub enable_heartbeat: bool,
    /// 心跳檢測間隔（預設 5 秒）
    pub heartbeat_interval: Duration,
}

impl Default for ProxyDaemonConfig {
    fn default() -> Self {
        Self {
            restart_delay: Duration::from_millis(3000),
            max_restart_attempts: 5,
            restart_attempt_reset_time: Duration::from_millis(60000),
            enable_heartbeat: true,
            heartbeat_interval: Duration::from_millis(5000),
        }
    }
}

// ─── 內部狀態 ───────────────────────────────────────────────────────

/// 守護邏輯的簿記狀態（不含子進程句柄——子進程在共享 cell 中）
struct DaemonBookkeeping {
    /// 是否正在關閉中（阻止重啟）
    is_shutting_down: bool,
    /// 連續重啟次數
    restart_attempts: u32,
    /// 上次重啟時間
    #[allow(dead_code)]
    last_restart_time: Option<Instant>,
    /// 重啟計數重置定時器（到期後自動歸零）
    reset_timer: Option<tokio::task::JoinHandle<()>>,
}

// ─── 守護程序主體 ───────────────────────────────────────────────────

/// Proxy 守護程序
///
/// 透過 `Arc<ProxyDaemon>` 共享。子進程句柄獨立存於 `child_cell`（同時由
/// `tauri::State` 持有），確保退出時能可靠 kill。
pub struct ProxyDaemon {
    config: ProxyDaemonConfig,
    bookkeeping: Arc<AsyncMutex<DaemonBookkeeping>>,
    /// 共享子進程句柄（與 managed state 同一個 Arc）
    child_cell: ChildCell,
    app: AppHandle,
}

impl ProxyDaemon {
    /// 建立守護程序實例，綁定到預先建立的 `child_cell`
    ///
    /// `child_cell` 應由 `lib.rs::setup()` 預先建立並 `manage()`，
    /// 確保退出處理隨時能存取子進程句柄。
    pub fn new(app: AppHandle, config: ProxyDaemonConfig, child_cell: ChildCell) -> Arc<Self> {
        Arc::new(Self {
            config,
            bookkeeping: Arc::new(AsyncMutex::new(DaemonBookkeeping {
                is_shutting_down: false,
                restart_attempts: 0,
                last_restart_time: None,
                reset_timer: None,
            })),
            child_cell,
            app,
        })
    }

    /// 啟動 sidecar 並開始守護
    pub async fn start(self: &Arc<Self>) -> Result<(), String> {
        self.spawn_sidecar().await?;

        if self.config.enable_heartbeat {
            let daemon = Arc::clone(self);
            tokio::spawn(async move {
                daemon.heartbeat_loop().await;
            });
        }

        log::info!("Proxy 守護程序已啟動");
        Ok(())
    }

    // ─── sidecar 管理 ───────────────────────────────────────────────

    /// 生成 sidecar 進程並設定事件監聽
    async fn spawn_sidecar(self: &Arc<Self>) -> Result<(), String> {
        let sidecar = self
            .app
            .shell()
            .sidecar("proxy")
            .map_err(|e| format!("建立 sidecar 命令失敗: {e}"))?;

        let (rx, child) = sidecar
            .spawn()
            .map_err(|e| format!("啟動 sidecar 失敗: {e}"))?;

        // 存入共享 cell（清理舊進程）
        {
            let mut cell = self.child_cell.lock().expect("child_cell poisoned");
            if let Some(old) = cell.take() {
                let _ = old.kill();
            }
            *cell = Some(child);
        }

        // 啟動事件監聽任務
        let daemon = Arc::clone(self);
        tokio::spawn(async move {
            daemon.event_listener(rx).await;
        });

        log::info!("Proxy sidecar 已啟動");
        Ok(())
    }

    // ─── 事件監聽 ───────────────────────────────────────────────────

    /// 監聽 sidecar 的 Stdout / Stderr / Terminated / Error 事件
    async fn event_listener(
        self: &Arc<Self>,
        mut rx: tokio::sync::mpsc::Receiver<CommandEvent>,
    ) {
        while let Some(event) = rx.recv().await {
            match event {
                CommandEvent::Stdout(line) => {
                    let output = String::from_utf8_lossy(&line);
                    for line in output.lines() {
                        log::info!("[proxy] {line}");
                    }
                }
                CommandEvent::Stderr(line) => {
                    let output = String::from_utf8_lossy(&line);
                    for line in output.lines() {
                        log::warn!("[proxy stderr] {line}");
                    }
                }
                CommandEvent::Terminated(TerminatedPayload { code, signal }) => {
                    let is_shutting_down = {
                        let bk = self.bookkeeping.lock().await;
                        bk.is_shutting_down
                    };

                    // 清除共享 cell 中已退出的句柄
                    {
                        let mut cell = self.child_cell.lock().expect("child_cell poisoned");
                        *cell = None;
                    }

                    if is_shutting_down {
                        log::info!("Proxy 進程正常關閉 (code: {code:?}, signal: {signal:?})");
                        return;
                    }

                    log::warn!("Proxy 進程意外退出 (code: {code:?}, signal: {signal:?})");
                    self.handle_process_exit();
                    return;
                }
                CommandEvent::Error(msg) => {
                    log::error!("Proxy 進程錯誤: {msg}");
                }
                _ => {
                    // CommandEvent 標記為 #[non_exhaustive]，未來可能新增變體
                }
            }
        }
        // channel 關閉（進程已結束但未收到 Terminated）
        let is_shutting_down = {
            let bk = self.bookkeeping.lock().await;
            bk.is_shutting_down
        };
        if !is_shutting_down {
            // 清除 cell
            {
                let mut cell = self.child_cell.lock().expect("child_cell poisoned");
                *cell = None;
            }
            log::warn!("Proxy 事件通道已關閉（未收到 Terminated），嘗試重啟");
            self.handle_process_exit();
        }
    }

    // ─── 心跳檢測 ───────────────────────────────────────────────────

    /// 定期檢查 sidecar 進程是否存活
    ///
    /// 透過 PID + 平台 API 判斷進程是否仍存在。
    /// 若已死亡，觸發 `handle_process_exit` 進行重啟。
    async fn heartbeat_loop(self: &Arc<Self>) {
        let mut ticker = interval(self.config.heartbeat_interval);
        ticker.tick().await; // 跳過第一次立即觸發

        loop {
            ticker.tick().await;

            let (pid, is_shutting_down) = {
                let bk = self.bookkeeping.lock().await;
                let pid = self
                    .child_cell
                    .lock()
                    .expect("child_cell poisoned")
                    .as_ref()
                    .map(|c| c.pid());
                (pid, bk.is_shutting_down)
            };

            if is_shutting_down {
                return;
            }

            match pid {
                Some(pid) if !is_process_alive(pid) => {
                    log::warn!("心跳檢測：Proxy 進程 (pid={pid}) 已不存在");
                    // 清除失效句柄
                    {
                        let mut cell = self.child_cell.lock().expect("child_cell poisoned");
                        *cell = None;
                    }
                    self.handle_process_exit();
                    return;
                }
                None => {
                    log::warn!("心跳檢測：無可用的 Proxy 進程");
                    self.handle_process_exit();
                    return;
                }
                _ => { /* 進程存活，繼續 */ }
            }
        }
    }

    // ─── 退出處理 & 重啟 ────────────────────────────────────────────

    /// 處理進程異常退出：檢查重啟次數 → 調度重啟 or 強制退出應用
    ///
    /// 此方法是同步的——它只讀寫 state 並 spawn async task 來執行實際重啟，
    /// 避免 async fn 類型遞迴循環。
    fn handle_process_exit(self: &Arc<Self>) {
        let daemon = Arc::clone(self);

        tokio::spawn(async move {
            let attempts;
            let delay;

            {
                let mut bk = daemon.bookkeeping.lock().await;

                // 達到上限 → 退出應用
                if bk.restart_attempts >= daemon.config.max_restart_attempts {
                    log::error!(
                        "Proxy 進程頻繁異常退出，已達最大重啟次數 ({})，應用即將退出",
                        daemon.config.max_restart_attempts
                    );
                    let app = daemon.app.clone();
                    tokio::spawn(async move {
                        tokio::time::sleep(Duration::from_millis(500)).await;
                        app.exit(1);
                    });
                    return;
                }

                // 計數 +1
                bk.restart_attempts += 1;
                bk.last_restart_time = Some(Instant::now());
                attempts = bk.restart_attempts;
                delay = daemon.config.restart_delay;

                // 重置之前的 reset timer
                if let Some(timer) = bk.reset_timer.take() {
                    timer.abort();
                }

                // 啟動新的 reset timer
                let bk_arc = Arc::clone(&daemon.bookkeeping);
                let reset_duration = daemon.config.restart_attempt_reset_time;
                let timer = tokio::spawn(async move {
                    tokio::time::sleep(reset_duration).await;
                    let mut s = bk_arc.lock().await;
                    if s.restart_attempts > 0 {
                        log::info!("重啟計數已重置 (之前: {} 次)", s.restart_attempts);
                        s.restart_attempts = 0;
                    }
                });
                bk.reset_timer = Some(timer);
            }

            log::info!("準備重啟 Proxy 進程 (第 {attempts} 次嘗試)...");

            tokio::time::sleep(delay).await;
            match daemon.spawn_sidecar().await {
                Ok(()) => log::info!("Proxy 進程重啟成功"),
                Err(e) => log::error!("Proxy 進程重啟失敗: {e}"),
            }
        });
    }

    // ─── 優雅關閉（同步，可在無 tokio runtime 時呼叫）──────────────

    /// **同步**關閉 sidecar 進程——防孤兒機制的關鍵路徑。
    ///
    /// 此方法設計為可從 `RunEvent::ExitRequested` / `RunEvent::Exit` 主執行緒回調中
    /// 直接呼叫，**不依賴 tokio runtime 仍存活**：
    /// 1. 標記 shutting down（阻止重啟，用 try_lock 容忍競態）
    /// 2. 從共享 cell 取出子進程句柄
    /// 3. 同步 `kill()`
    ///
    /// 若 bookkeeping 鎖被佔用（例如重啟邏輯正在跑），不阻塞退出——
    /// 因為 kill 子進程才是最關鍵的，重啟旗標在進程退出後已無意義。
    pub fn kill_child_sync(&self) {
        // 嘗試標記關閉中（非阻塞，失敗也不影響 kill）
        if let Ok(mut bk) = self.bookkeeping.try_lock() {
            bk.is_shutting_down = true;
            if let Some(timer) = bk.reset_timer.take() {
                timer.abort();
            }
        } else {
            log::warn!("bookkeeping 鎖被佔用，跳過關閉旗標設定（仍會 kill 子進程）");
        }

        // 取出並 kill 子進程（同步，持鎖極短）
        let child = {
            let mut cell = self.child_cell.lock().expect("child_cell poisoned");
            cell.take()
        };

        if let Some(child) = child {
            match child.kill() {
                Ok(()) => log::info!("Proxy 進程已終止 (sync kill)"),
                Err(e) => log::warn!("關閉 Proxy 進程時出錯: {e}"),
            }
        } else {
            log::info!("Proxy 子進程不存在或已退出（無需 kill）");
        }
    }
}

// ─── 跨平台進程存活檢測 ─────────────────────────────────────────────

/// 檢查指定 PID 的進程是否仍在運行
///
/// - Unix: `kill(pid, 0)` — 不發送信號，僅驗證進程存在
/// - Windows: `OpenProcess` + `GetExitCodeProcess` — 讀取退出碼判斷是否 `STILL_ACTIVE`
fn is_process_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        // SAFETY: kill(pid, 0) 為 POSIX 標準調用，不發送信號
        unsafe { libc::kill(pid as i32, 0) == 0 }
    }

    #[cfg(windows)]
    {
        windows_check_process_alive(pid)
    }

    #[cfg(not(any(unix, windows)))]
    {
        let _ = pid;
        true
    }
}

#[cfg(windows)]
fn windows_check_process_alive(pid: u32) -> bool {
    use core::ffi::c_void;

    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
    const STILL_ACTIVE: u32 = 259;

    type Handle = *mut c_void;
    const NULL_HANDLE: Handle = core::ptr::null_mut();

    extern "system" {
        fn OpenProcess(
            dw_desired_access: u32,
            b_inherit_handle: i32,
            dw_process_id: u32,
        ) -> Handle;
        fn GetExitCodeProcess(h_process: Handle, lp_exit_code: *mut u32) -> i32;
        fn CloseHandle(hObject: Handle) -> i32;
    }

    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle == NULL_HANDLE {
            return false;
        }
        let mut exit_code: u32 = 0;
        let ok = GetExitCodeProcess(handle, &mut exit_code);
        CloseHandle(handle);
        ok != 0 && exit_code == STILL_ACTIVE
    }
}

// ─── Tauri 整合函數 ─────────────────────────────────────────────────

/// 從 `lib.rs::setup()` 呼叫：建立守護程序並啟動 sidecar。
///
/// `child_cell` 必須是已在 `setup()` 中 `manage()` 的同一個容器，
/// 這樣退出處理才能透過 `tauri::State<ChildCell>` 找到子進程。
pub async fn start_proxy_daemon(
    app: &AppHandle,
    child_cell: ChildCell,
) -> Result<Arc<ProxyDaemon>, String> {
    let daemon = ProxyDaemon::new(app.clone(), ProxyDaemonConfig::default(), child_cell);
    daemon.start().await?;
    Ok(daemon)
}

/// **同步** kill 所有 sidecar 子進程——供 `RunEvent::ExitRequested` / `Exit` 呼叫。
///
/// 從 `tauri::State` 取出共享 cell，直接 kill 當前子進程。
/// 此函數不依賴 tokio runtime，可在主執行緒退出階段安全呼叫。
pub fn kill_sidecar_sync(app: &AppHandle) {
    use tauri::Manager;
    if let Some(cell) = app.try_state::<ChildCell>() {
        let child = {
            let mut guard = cell.inner().lock().expect("child_cell poisoned");
            guard.take()
        };
        if let Some(child) = child {
            match child.kill() {
                Ok(()) => log::info!("Proxy 進程已終止 (exit handler)"),
                Err(e) => log::warn!("exit handler kill Proxy 進程失敗: {e}"),
            }
        }
    } else {
        // cell 尚未 manage（極早期退出）——嘗試從 daemon 取得
        if let Some(daemon) = app.try_state::<Arc<ProxyDaemon>>() {
            daemon.inner().kill_child_sync();
        } else {
            log::warn!("找不到 Proxy 子進程狀態，可能從未啟動");
        }
    }
}
