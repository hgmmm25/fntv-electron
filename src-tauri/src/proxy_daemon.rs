//! Proxy 守護程序模組 — 管理 Go proxy sidecar 的生命週期
//!
//! 完整移植自 `src/main/common/proxyDaemon.ts`，包含：
//! - 進程啟動 / 監控 / 重啟 / 關閉
//! - 心跳檢測（定期驗證進程存活）
//! - 異常退出後自動重啟（可配置最大次數與重置時間）

use std::sync::Arc;
use std::time::{Duration, Instant};
use tauri::AppHandle;
use tauri_plugin_shell::process::{CommandChild, CommandEvent, TerminatedPayload};
use tauri_plugin_shell::ShellExt;
use tokio::sync::Mutex;
use tokio::time::interval;

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

struct DaemonState {
    /// 當前子進程句柄（持有產權，kill 時需 take）
    child: Option<CommandChild>,
    /// 是否正在關閉中
    is_shutting_down: bool,
    /// 連續重啟次數
    restart_attempts: u32,
    /// 上次重啟時間
    last_restart_time: Option<Instant>,
    /// 重啟計數重置定時器（到期後自動歸零）
    reset_timer: Option<tokio::task::JoinHandle<()>>,
}

// ─── 守護程序主體 ───────────────────────────────────────────────────

/// Proxy 守護程序
///
/// 透過 `Arc<ProxyDaemon>` 共享，存放於 Tauri managed state。
/// 所有進程操作均為非同步，由 tokio runtime 驅動。
pub struct ProxyDaemon {
    config: ProxyDaemonConfig,
    state: Arc<Mutex<DaemonState>>,
    app: AppHandle,
}

impl ProxyDaemon {
    /// 建立新的守護程序實例（不自動啟動）
    pub fn new(app: AppHandle, config: ProxyDaemonConfig) -> Arc<Self> {
        Arc::new(Self {
            config,
            state: Arc::new(Mutex::new(DaemonState {
                child: None,
                is_shutting_down: false,
                restart_attempts: 0,
                last_restart_time: None,
                reset_timer: None,
            })),
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

        // 存入 state（清理舊進程）
        {
            let mut state = self.state.lock().await;
            if let Some(old) = state.child.take() {
                let _ = old.kill();
            }
            state.child = Some(child);
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
                        let state = self.state.lock().await;
                        state.is_shutting_down
                    };

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
            let state = self.state.lock().await;
            state.is_shutting_down
        };
        if !is_shutting_down {
            log::warn!("Proxy 事件通道已關閉（未收到 Terminated），嘗試重啟");
            self.handle_process_exit();
        }
    }

    // ─── 心跳檢測 ───────────────────────────────────────────────────

    /// 定期檢查 sidecar 進程是否存活
    ///
    /// 與 TypeScript 版本一致：透過 PID + 平台 API 判斷進程是否仍存在。
    /// 若已死亡，觸發 `handle_process_exit` 進行重啟。
    async fn heartbeat_loop(self: &Arc<Self>) {
        let mut ticker = interval(self.config.heartbeat_interval);
        ticker.tick().await; // 跳過第一次立即觸發

        loop {
            ticker.tick().await;

            let (pid, is_shutting_down) = {
                let state = self.state.lock().await;
                let pid = state.child.as_ref().map(|c| c.pid());
                (pid, state.is_shutting_down)
            };

            if is_shutting_down {
                return;
            }

            match pid {
                Some(pid) if !is_process_alive(pid) => {
                    log::warn!("心跳檢測：Proxy 進程 (pid={pid}) 已不存在");
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
    /// 此方法是同步的——它只讀寫 state 並 spawn async task 來執行實際重啟。
    /// 這避免了 `handle_process_exit` ↔ `spawn_sidecar` ↔ `event_listener`
    /// 之間的 async fn 類型遞迴循環。
    fn handle_process_exit(self: &Arc<Self>) {
        let daemon = Arc::clone(self);

        // 在背景 task 中執行（避免阻塞事件監聽器）
        tokio::spawn(async move {
            let should_restart;
            let attempts;
            let delay;

            {
                let mut state = daemon.state.lock().await;

                // 清除子進程
                state.child = None;

                // 達到上限 → 退出應用
                if state.restart_attempts >= daemon.config.max_restart_attempts {
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
                state.restart_attempts += 1;
                state.last_restart_time = Some(Instant::now());
                attempts = state.restart_attempts;
                delay = daemon.config.restart_delay;

                // 重置之前的 reset timer
                if let Some(timer) = state.reset_timer.take() {
                    timer.abort();
                }

                // 啟動新的 reset timer
                let state_clone = Arc::clone(&daemon.state);
                let reset_duration = daemon.config.restart_attempt_reset_time;
                let timer = tokio::spawn(async move {
                    tokio::time::sleep(reset_duration).await;
                    let mut s = state_clone.lock().await;
                    if s.restart_attempts > 0 {
                        log::info!("重啟計數已重置 (之前: {} 次)", s.restart_attempts);
                        s.restart_attempts = 0;
                    }
                });
                state.reset_timer = Some(timer);

                should_restart = true;
            }

            if !should_restart {
                return;
            }

            log::info!("準備重啟 Proxy 進程 (第 {attempts} 次嘗試)...");

            tokio::time::sleep(delay).await;
            match daemon.spawn_sidecar().await {
                Ok(()) => log::info!("Proxy 進程重啟成功"),
                Err(e) => log::error!("Proxy 進程重啟失敗: {e}"),
            }
        });
    }

    // ─── 優雅關閉 ───────────────────────────────────────────────────

    /// 關閉守護程序與 sidecar 進程
    ///
    /// 對應 TypeScript 版本的 `shutdown()` 方法：
    /// 1. 設定 `is_shutting_down` 標誌（停止重啟邏輯）
    /// 2. 清除 reset timer
    /// 3. 發送 kill 給子進程
    pub async fn shutdown(&self) {
        log::info!("Proxy 守護程序正在關閉...");

        let child = {
            let mut state = self.state.lock().await;
            state.is_shutting_down = true;

            // 停止 reset timer
            if let Some(timer) = state.reset_timer.take() {
                timer.abort();
            }

            // 取出子進程句柄（CommandChild::kill 取得產權）
            state.child.take()
        };

        // 關閉子進程
        if let Some(child) = child {
            match child.kill() {
                Ok(()) => log::info!("Proxy 進程已終止"),
                Err(e) => log::warn!("關閉 Proxy 進程時出錯: {e}"),
            }
        }

        log::info!("Proxy 守護程序已關閉");
    }

    // ─── 查詢介面 ───────────────────────────────────────────────────
    // 這些方法預留給未來的 Tauri commands（從前端查詢 proxy 狀態）使用。

    /// 當前重啟嘗試次數
    #[allow(dead_code)]
    pub async fn restart_attempts(&self) -> u32 {
        let state = self.state.lock().await;
        state.restart_attempts
    }

    /// 是否正在關閉中
    #[allow(dead_code)]
    pub async fn is_shutting_down(&self) -> bool {
        let state = self.state.lock().await;
        state.is_shutting_down
    }

    /// 當前 sidecar 的 PID（若正在運行）
    #[allow(dead_code)]
    pub async fn pid(&self) -> Option<u32> {
        let state = self.state.lock().await;
        state.child.as_ref().map(|c| c.pid())
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

/// 從 `lib.rs::setup()` 呼叫：建立守護程序、啟動 sidecar、存入 managed state
pub async fn start_proxy_daemon(app: &AppHandle) -> Result<Arc<ProxyDaemon>, String> {
    let daemon = ProxyDaemon::new(app.clone(), ProxyDaemonConfig::default());
    daemon.start().await?;
    Ok(daemon)
}

/// 從 `RunEvent::ExitRequested` 呼叫：優雅關閉 sidecar
pub async fn shutdown_proxy_daemon(app: &AppHandle) {
    use tauri::Manager;
    if let Some(daemon) = app.try_state::<Arc<ProxyDaemon>>() {
        daemon.inner().shutdown().await;
    }
}
