//! Proxy 守護程序模組 — 管理 Go proxy sidecar 的生命週期
//!
//! 完整移植自 `src/main/common/proxyDaemon.ts`，包含：
//! - 進程啟動 / 監控 / 重啟 / 關閉
//! - 心跳檢測（定期驗證進程存活）
//! - 異常退出後自動重啟（可配置最大次數與重置時間）
//! - 防孤兒進程機制（共享子進程句柄，退出時同步強制 kill）
//! - **優雅關閉**（先嘗試 SIGTERM，等待超時後 SIGKILL）
//! - **端口衝突自動處理**（探測可用埠、傳遞給 Go proxy、啟動後驗證）
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
//!
//! ## 優雅關閉策略
//!
//! 退出時不直接 SIGKILL，而是：
//! 1. 先發送 SIGTERM（Unix only），讓 Go 進程有機會釋放資源
//! 2. 每 100ms 輪詢進程存活狀態，最多等 3 秒
//! 3. 若仍在存活，改用 SIGKILL 強制終止
//!
//! ## 端口衝突處理
//!
//! Go proxy 預設監聽 22345，若被佔用：
//! 1. 從 preferred 端口開始掃描，找第一個可用埠（最多 10 個）
//! 2. 透過 `--port` 參數傳遞實際端口給 Go sidecar
//! 3. 啟動後以 TCP 連接探測驗證埠是否真的在監聽
//! 4. 透過 Tauri event 通知前端實際使用的端口

use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Manager};
use tauri_plugin_shell::process::{CommandChild, CommandEvent, TerminatedPayload};
use tauri_plugin_shell::ShellExt;
use tokio::sync::Mutex as AsyncMutex;
use tokio::time::interval;

/// Go proxy 認證密鑰檔案名（對應 Electron `userDataDir/proxy-secret`）。
/// 由 Rust 側生成/持久化，透過 stdin 注入 Go sidecar。
const PROXY_SECRET_FILE: &str = "proxy-secret";

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

// ─── 連接埠工具 ─────────────────────────────────────────────────────

/// Go proxy 預設監聽埠號
pub const DEFAULT_PROXY_PORT: u16 = 22345;

/// 檢查指定埠是否可用（綁定 TCP 來驗證）
fn is_port_available(port: u16) -> bool {
    TcpListener::bind(("127.0.0.1", port)).is_ok()
}

/// 從 `preferred` 開始，找到一個可用的埠。
/// 回傳 `None` 表示 `preferred..preferred+max_attempts` 範圍內全部被佔用。
fn find_available_port(preferred: u16, max_attempts: u16) -> Option<u16> {
    for i in 0..max_attempts {
        let port = preferred.wrapping_add(i);
        if is_port_available(port) {
            return Some(port);
        }
    }
    None
}

/// 啟動後驗證埠是否真的在監聽（TCP 連接探測）。
/// 回傳 `true` 表示埠可連接（Go 進程已在監聽）。
fn verify_port_listening(port: u16, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        match TcpStream::connect(("127.0.0.1", port)) {
            Ok(stream) => {
                drop(stream);
                return true;
            }
            Err(_) => {
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }
    false
}

// ─── 代理認證密鑰 ─────────────────────────────────────────────────

/// 產生 64 hex（32 bytes）隨機密鑰，對應 Electron `randomBytes(32).toString('hex')`。
#[cfg(windows)]
fn generate_random_secret() -> Result<String, String> {
    use windows_sys::Win32::Security::Cryptography::BCryptGenRandom;
    const BCRYPT_USE_SYSTEM_PREFERRED_RNG: u32 = 0x2;
    let mut buf = [0u8; 32];
    let status = unsafe {
        BCryptGenRandom(
            std::ptr::null_mut(),
            buf.as_mut_ptr(),
            buf.len() as u32,
            BCRYPT_USE_SYSTEM_PREFERRED_RNG,
        )
    };
    if status != 0 {
        return Err(format!("BCryptGenRandom 失敗 (NTSTATUS=0x{status:x})"));
    }
    Ok(hex::encode(buf))
}

/// 非 Windows 後備（讀 /dev/urandom）
#[cfg(not(windows))]
fn generate_random_secret() -> Result<String, String> {
    use std::io::Read;
    let mut buf = [0u8; 32];
    let mut f = std::fs::File::open("/dev/urandom")
        .map_err(|e| format!("無法開啟 /dev/urandom: {e}"))?;
    f.read_exact(&mut buf)
        .map_err(|e| format!("讀取 /dev/urandom 失敗: {e}"))?;
    Ok(hex::encode(buf))
}

/// 讀取既有密鑰（僅接受 64 hex，防止格式錯誤的殘留檔案導致啟動失敗）
fn read_existing_secret(path: &std::path::Path) -> Option<String> {
    let content = std::fs::read_to_string(path).ok()?;
    let trimmed = content.trim().to_string();
    let is_valid = trimmed.len() == 64
        && trimmed
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase());
    if is_valid {
        Some(trimmed)
    } else {
        log::warn!("proxy-secret 檔案格式無效，重新生成");
        None
    }
}

/// 在指定目錄下載入或建立 proxy-secret，回傳密鑰字串。
fn load_or_create_secret_in_dir(data_dir: &std::path::Path) -> Result<String, String> {
    std::fs::create_dir_all(data_dir).map_err(|e| format!("無法建立 app_data_dir: {e}"))?;

    let secret_path = data_dir.join(PROXY_SECRET_FILE);
    if let Some(existing) = read_existing_secret(&secret_path) {
        return Ok(existing);
    }

    let secret = generate_random_secret()?;

    // 原子寫入：先寫臨時檔再 rename，避免半寫狀態
    let tmp_path = data_dir.join(format!("{PROXY_SECRET_FILE}.tmp"));
    std::fs::write(&tmp_path, format!("{secret}\n"))
        .map_err(|e| format!("寫入 proxy-secret 失敗: {e}"))?;
    std::fs::rename(&tmp_path, &secret_path)
        .map_err(|e| format!("取代 proxy-secret 失敗: {e}"))?;
    log::info!("已建立 proxy-secret: {}", secret_path.display());
    Ok(secret)
}

/// 在 app_data_dir 下載入或建立 proxy-secret，回傳密鑰字串。
fn load_or_create_proxy_secret(app: &AppHandle) -> Result<String, String> {
    let data_dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("無法取得 app_data_dir: {e}"))?;
    load_or_create_secret_in_dir(&data_dir)
}

#[cfg(test)]
mod secret_tests {
    use super::*;

    #[test]
    fn generate_secret_is_64_lower_hex() {
        let secret = generate_random_secret().unwrap();
        assert_eq!(secret.len(), 64);
        assert!(secret
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
    }

    #[test]
    fn load_or_create_reuses_existing_file() {
        let dir = std::env::temp_dir().join(format!(
            "fntv-proxy-secret-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let first = load_or_create_secret_in_dir(&dir).unwrap();
        assert_eq!(first.len(), 64);
        let second = load_or_create_secret_in_dir(&dir).unwrap();
        assert_eq!(first, second, "再次載入應複用同一密鑰");
        let persisted = std::fs::read_to_string(dir.join(PROXY_SECRET_FILE)).unwrap();
        assert_eq!(persisted.trim(), first);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn invalid_existing_file_is_replaced() {
        let dir = std::env::temp_dir().join(format!(
            "fntv-proxy-secret-test-invalid-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(PROXY_SECRET_FILE), "not-a-valid-secret").unwrap();
        let secret = load_or_create_secret_in_dir(&dir).unwrap();
        assert_eq!(secret.len(), 64);
        let _ = std::fs::remove_dir_all(&dir);
    }
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

// ─── 優雅關閉 ───────────────────────────────────────────────────────

/// 給定一個 `CommandChild`，先嘗試 SIGTERM 優雅關閉，超時後 SIGKILL 強制終止。
///
/// 此函數為同步設計，可在主執行緒 `RunEvent::ExitRequested` / `Exit` 回調中安全呼叫，
/// 不依賴 tokio runtime。
///
/// - **Unix**：先發送 `SIGTERM`，每 100ms 輪詢存活狀態，最多等 3 秒，仍存活則 `SIGKILL`
/// - **Windows**：無 `SIGTERM` 等效機制，直接 `TerminateProcess`
fn graceful_kill_child(child: CommandChild) {
    let pid = child.pid();

    // ── Unix：先嘗試 SIGTERM，讓 Go 進程有機會優雅退出 ──
    #[cfg(unix)]
    {
        // SAFETY: SIGTERM 為 POSIX 標準信號，kill(pid, SIGTERM) 為標準調用
        unsafe {
            libc::kill(pid as i32, libc::SIGTERM);
        }
        log::info!("已發送 SIGTERM 到 Proxy 進程 (pid={pid})，等待優雅退出...");

        // 每 100ms 輪詢，最多等 3 秒
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            if !is_process_alive(pid) {
                log::info!("Proxy 進程已優雅退出 (SIGTERM, pid={pid})");
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }

        log::warn!("Proxy 進程未在 3 秒內回應 SIGTERM (pid={pid})，改用 SIGKILL 強制終止");
    }

    // ── 強制終止：SIGKILL (Unix) / TerminateProcess (Windows) ──
    match child.kill() {
        Ok(()) => log::info!("Proxy 進程已強制終止 (pid={pid})"),
        Err(e) => log::warn!("強制終止 Proxy 進程時出錯 (pid={pid}): {e}"),
    }
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
    /// Go proxy 實際使用的連接埠（可能與 DEFAULT_PROXY_PORT 不同）
    actual_port: AtomicU16,
    /// 代理認證密鑰（64 hex，與 Go sidecar 共享，用於 /api/v1/session 鑑權）
    secret: String,
}

impl ProxyDaemon {
    /// 建立守護程序實例，綁定到預先建立的 `child_cell`
    ///
    /// `child_cell` 應由 `lib.rs::setup()` 預先建立並 `manage()`，
    /// 確保退出處理隨時能存取子進程句柄。
    pub fn new(app: AppHandle, config: ProxyDaemonConfig, child_cell: ChildCell) -> Arc<Self> {
        let secret = load_or_create_proxy_secret(&app).unwrap_or_else(|e| {
            log::error!("初始化 proxy-secret 失敗: {e}");
            String::new()
        });
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
            actual_port: AtomicU16::new(DEFAULT_PROXY_PORT),
            secret,
        })
    }

    /// 回傳 Go proxy 實際使用的埠號
    pub fn port(&self) -> u16 {
        self.actual_port.load(Ordering::Relaxed)
    }

    /// 回傳 Go proxy 的基礎 URL
    pub fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port())
    }

    /// 回傳代理認證密鑰（供播放流程註冊 proxy session）
    pub fn secret(&self) -> &str {
        &self.secret
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
    ///
    /// 流程：
    /// 1. 從 `actual_port` 開始探測可用埠
    /// 2. 透過 `--port` 參數傳遞埠號給 Go sidecar
    /// 3. 存入共享 cell
    /// 4. 啟動後以 TCP 探測驗證埠是否真的在監聽
    async fn spawn_sidecar(self: &Arc<Self>) -> Result<(), String> {
        // 1. 找可用埠（從當前已知埠開始，最多掃描 10 個）
        let preferred = self.actual_port.load(Ordering::Relaxed);
        let port = find_available_port(preferred, 10).ok_or_else(|| {
            format!(
                "連接埠 {preferred}~{} 全部被佔用，無法啟動 Proxy",
                preferred + 9
            )
        })?;

        // 2. 構建 sidecar 命令並傳遞埠號
        let sidecar = self
            .app
            .shell()
            .sidecar("proxy")
            .map_err(|e| format!("建立 sidecar 命令失敗: {e}"))?
            .args(["--port", &port.to_string()]);

        let (rx, mut child) = sidecar
            .spawn()
            .map_err(|e| format!("啟動 sidecar 失敗: {e}"))?;

        // 2.5 透過 stdin 注入認證密鑰（上游協議：Go 啟動時從 stdin 讀一行 secret）
        let secret = self.secret.clone();
        if secret.is_empty() {
            let _ = child.kill();
            return Err("proxy-secret 為空，無法啟動 Proxy".to_string());
        }
        if let Err(e) = child.write(format!("{secret}\n").as_bytes()) {
            log::warn!("寫入 proxy secret 至 stdin 失敗: {e}");
        }

        // 3. 存入共享 cell（清理舊進程）
        {
            let mut cell = self.child_cell.lock().expect("child_cell poisoned");
            if let Some(old) = cell.take() {
                let _ = old.kill();
            }
            *cell = Some(child);
        }

        // 4. 更新實際埠號
        self.actual_port.store(port, Ordering::Relaxed);

        // 5. 啟動事件監聽任務
        let daemon = Arc::clone(self);
        tokio::spawn(async move {
            daemon.event_listener(rx).await;
        });

        // 6. 驗證埠是否真的在監聽（最多等 5 秒）
        if verify_port_listening(port, Duration::from_secs(5)) {
            log::info!("Proxy sidecar 已啟動，監聽於 127.0.0.1:{port}");
        } else {
            log::warn!(
                "Proxy sidecar 啟動後未能驗證埠 {port} 監聽狀態（可能仍在初始化或啟動失敗）"
            );
        }

        // 7. 通知前端實際端口
        #[cfg(desktop)]
        {
            use tauri::Emitter;
            let _ = self.app.emit("proxy-started", port);
        }

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

    // ─── 退出處理（同步，可在無 tokio runtime 時呼叫）───────────────

    /// **同步**優雅關閉 sidecar 進程——防孤兒機制的關鍵路徑。
    ///
    /// 此方法設計為可從 `RunEvent::ExitRequested` / `RunEvent::Exit` 主執行緒回調中
    /// 直接呼叫，**不依賴 tokio runtime 仍存活**：
    /// 1. 標記 shutting down（阻止重啟，用 try_lock 容忍競態）
    /// 2. 從共享 cell 取出子進程句柄
    /// 3. 優雅關閉：SIGTERM → 輪詢等待 → SIGKILL
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

        // 取出子進程
        let child = {
            let mut cell = self.child_cell.lock().expect("child_cell poisoned");
            cell.take()
        };

        if let Some(child) = child {
            graceful_kill_child(child);
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

/// **同步**優雅關閉 sidecar 進程——供 `RunEvent::ExitRequested` / `Exit` 呼叫。
///
/// 從 `tauri::State` 取出共享 cell，先嘗試 SIGTERM 再 SIGKILL 強制終止。
/// 此函數不依賴 tokio runtime，可在主執行緒退出階段安全呼叫。
pub fn kill_sidecar_sync(app: &AppHandle) {
    use tauri::Manager;

    // 嘗試直接從 ChildCell 取出子進程
    let child = app
        .try_state::<ChildCell>()
        .and_then(|cell| {
            let mut guard = cell.inner().lock().expect("child_cell poisoned");
            guard.take()
        });

    if let Some(child) = child {
        graceful_kill_child(child);
        return;
    }

    // Fallback：從 ProxyDaemon 取出（cell 尚未 manage 或已被清空的極端情況）
    if let Some(daemon) = app.try_state::<Arc<ProxyDaemon>>() {
        daemon.inner().kill_child_sync();
    } else {
        log::warn!("找不到 Proxy 子進程狀態，可能從未啟動");
    }
}

/// Tauri 命令：回傳 Go proxy 的基礎 URL（含埠號）
///
/// 前端可透過 `invoke('get_proxy_base_url')` 查詢實際的 proxy 地址，
/// 避免硬編碼端口號。
#[tauri::command]
pub fn get_proxy_base_url(app: tauri::AppHandle) -> Result<String, String> {
    use tauri::Manager;
    if let Some(daemon) = app.try_state::<Arc<ProxyDaemon>>() {
        Ok(daemon.inner().base_url())
    } else {
        // Daemon 尚未啟動或啟動失敗，回傳預設值
        Ok(format!("http://127.0.0.1:{DEFAULT_PROXY_PORT}"))
    }
}
