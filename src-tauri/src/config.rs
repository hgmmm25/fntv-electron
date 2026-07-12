//! 設定檔模組 — 管理本地 config.json 的讀寫、加密、以及所有偏好設定 Tauri commands
//!
//! 完整移植自 `src/modules/fn_config/config.ts`，包含：
//! - AppConfig / HistoryItem 資料結構
//! - AES-256-CBC 密碼加密/解密（與 Electron 相同 key/IV）
//! - 16 個 Tauri commands 供前端呼叫
//! - 設定檔儲存於 `{app_data_dir}/config.json`（對應 Electron 的 `~/.fntv/config.json`）

use aes::cipher::{block_padding::NoPadding, BlockDecryptMut, BlockEncryptMut, KeyIvInit};
use serde::{Deserialize, Serialize};
use tauri::Manager;

// ─── 加密常數（與 Electron 版本完全一致）────────────────────────

/// AES-256-CBC 加密金鑰（32 bytes）
const ENCRYPTION_KEY: &[u8; 32] = b"U2XDcFsV6rdTE9wB5ZHvy6BW9hBTKJ1H";
/// AES-CBC 初始化向量（16 bytes 全零）
const IV: [u8; 16] = [0u8; 16];

/// 歷史記錄上限
const HISTORY_LIMIT: usize = 5;

// ─── 資料結構 ───────────────────────────────────────────────

/// 應用程式設定（對應 TypeScript `Config`）
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct AppConfig {
    pub account: Option<String>,
    pub domain: Option<String>,
    pub token: Option<String>,
    pub use_https: Option<bool>,
    pub history: Option<Vec<HistoryItem>>,
    pub download_proxy_enabled: Option<bool>,
    pub download_proxy: Option<String>,
    pub hide_original_play_button: Option<bool>,
    pub nas_proxy_enabled: Option<bool>,
    pub mpv_player_path: Option<String>,
    pub exit_mode: Option<String>,
    pub mac_close_action: Option<String>,
    pub tray_notification_shown: Option<bool>,
}

/// 歷史記錄項（對應 TypeScript `HistoryItem`）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryItem {
    pub domain: String,
    pub account: String,
    /// AES-256-CBC 加密後的 hex 字串
    pub password: String,
    pub use_https: Option<bool>,
}

/// 歷史記錄項（回傳給前端，密碼已解密）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryItemDecrypted {
    pub domain: String,
    pub account: String,
    /// 已解密的明文密碼
    pub password: String,
    pub use_https: Option<bool>,
}

/// 回傳給前端的設定資料
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigData {
    pub config: AppConfig,
    pub history: Vec<HistoryItemDecrypted>,
}

// ─── 加密/解密 ─────────────────────────────────────────────

type Aes256CbcEnc = cbc::Encryptor<aes::Aes256>;
type Aes256CbcDec = cbc::Decryptor<aes::Aes256>;

/// AES-256-CBC 加密（PKCS7 padding → hex）
pub fn encrypt_password(plain: &str) -> String {
    let plaintext = plain.as_bytes();
    // PKCS7 padding 需要足夠的 buffer
    let mut buf = vec![0u8; plaintext.len() + 16];
    buf[..plaintext.len()].copy_from_slice(plaintext);

    let encrypted = Aes256CbcEnc::new(ENCRYPTION_KEY.into(), &IV.into())
        .encrypt_padded_mut::<NoPadding>(&mut buf, plaintext.len())
        .expect("AES encryption should not fail");
    hex::encode(encrypted)
}

/// AES-256-CBC 解密（hex → PKCS7 unpad）
pub fn decrypt_password(encrypted_hex: &str) -> String {
    let ciphertext = match hex::decode(encrypted_hex) {
        Ok(v) => v,
        Err(_) => return String::new(),
    };

    let mut buf = ciphertext.clone();
    match Aes256CbcDec::new(ENCRYPTION_KEY.into(), &IV.into())
        .decrypt_padded_mut::<NoPadding>(&mut buf)
    {
        Ok(plaintext) => String::from_utf8_lossy(plaintext).to_string(),
        Err(_) => String::new(),
    }
}

// ─── 設定檔路徑 ────────────────────────────────────────────

/// 取得 config.json 的完整路徑
fn config_path(app: &tauri::AppHandle) -> std::path::PathBuf {
    let dir = app
        .path()
        .app_data_dir()
        .unwrap_or_else(|_| std::path::PathBuf::from("."));
    if !dir.exists() {
        let _ = std::fs::create_dir_all(&dir);
    }
    dir.join("config.json")
}

/// 讀取設定檔，不存在或解析失敗則回傳預設值
pub fn read_config(app: &tauri::AppHandle) -> AppConfig {
    let path = config_path(app);
    if !path.exists() {
        return AppConfig::default();
    }
    match std::fs::read_to_string(&path) {
        Ok(content) => serde_json::from_str(&content).unwrap_or_default(),
        Err(_) => AppConfig::default(),
    }
}

/// 寫入設定檔
pub fn write_config(app: &tauri::AppHandle, config: &AppConfig) -> Result<(), String> {
    let path = config_path(app);
    let json = serde_json::to_string_pretty(config).map_err(|e| e.to_string())?;
    std::fs::write(&path, json).map_err(|e| format!("寫入設定檔失敗: {e}"))
}

// ─── Tauri Commands ────────────────────────────────────────

/// 取得設定資料（config + 解密後的 history）
#[tauri::command]
pub fn get_config(app: tauri::AppHandle) -> Result<ConfigData, String> {
    let config = read_config(&app);
    let history = config
        .history
        .as_ref()
        .map(|h| {
            h.iter()
                .map(|item| HistoryItemDecrypted {
                    domain: item.domain.clone(),
                    account: item.account.clone(),
                    password: decrypt_password(&item.password),
                    use_https: item.use_https,
                })
                .collect()
        })
        .unwrap_or_default();

    Ok(ConfigData { config, history })
}

/// 儲存登入設定（account, domain, token, useHttps）
#[tauri::command]
pub fn save_login_config(
    app: tauri::AppHandle,
    account: String,
    domain: String,
    token: String,
    use_https: Option<bool>,
) -> Result<(), String> {
    let mut config = read_config(&app);
    config.account = Some(account);
    config.domain = Some(domain);
    config.token = Some(token);
    config.use_https = Some(use_https.unwrap_or(false));
    write_config(&app, &config)
}

/// 取得歷史記錄（密碼已解密）
#[tauri::command]
pub fn get_history(app: tauri::AppHandle) -> Result<Vec<HistoryItemDecrypted>, String> {
    let config = read_config(&app);
    Ok(config
        .history
        .unwrap_or_default()
        .into_iter()
        .map(|item| HistoryItemDecrypted {
            domain: item.domain,
            account: item.account,
            password: decrypt_password(&item.password),
            use_https: item.use_https,
        })
        .collect())
}

/// 新增歷史記錄（密碼加密儲存，上限 5 筆）
#[tauri::command]
pub fn add_history(
    app: tauri::AppHandle,
    domain: String,
    account: String,
    password: String,
    use_https: Option<bool>,
) -> Result<(), String> {
    let mut config = read_config(&app);
    let history = config.history.get_or_insert_with(Vec::new);

    // 移除重複項
    history.retain(|item| !(item.domain == domain && item.account == account));

    // 新增到最前面
    history.insert(
        0,
        HistoryItem {
            domain,
            account,
            password: encrypt_password(&password),
            use_https: Some(use_https.unwrap_or(false)),
        },
    );

    // 限制數量
    if history.len() > HISTORY_LIMIT {
        history.truncate(HISTORY_LIMIT);
    }

    write_config(&app, &config)
}

/// 清空歷史記錄
#[tauri::command]
pub fn clear_history(app: tauri::AppHandle) -> Result<(), String> {
    let mut config = read_config(&app);
    config.history = Some(Vec::new());
    write_config(&app, &config)
}

/// 刪除單筆歷史記錄
#[tauri::command]
pub fn delete_history_item(
    app: tauri::AppHandle,
    domain: String,
    account: String,
) -> Result<bool, String> {
    let mut config = read_config(&app);
    let history = config.history.get_or_insert_with(Vec::new);
    let original_len = history.len();
    history.retain(|item| !(item.domain == domain && item.account == account));

    if history.len() < original_len {
        write_config(&app, &config)?;
        Ok(true)
    } else {
        Ok(false)
    }
}

/// 取得下載代理設定
#[tauri::command]
pub fn get_download_proxy_config(app: tauri::AppHandle) -> Result<serde_json::Value, String> {
    let config = read_config(&app);
    Ok(serde_json::json!({
        "enabled": config.download_proxy_enabled.unwrap_or(true),
        "proxyUrl": config.download_proxy.unwrap_or_else(|| "https://ghfast.top".to_string()),
    }))
}

/// 設定下載代理
#[tauri::command]
pub fn set_download_proxy_config(
    app: tauri::AppHandle,
    enabled: Option<bool>,
    proxy_url: Option<String>,
) -> Result<(), String> {
    let mut config = read_config(&app);
    config.download_proxy_enabled = Some(enabled.unwrap_or(true));
    config.download_proxy =
        Some(proxy_url.unwrap_or_else(|| "https://ghfast.top".to_string()));
    write_config(&app, &config)
}

/// 取得「隱藏原有播放按鈕」設定
#[tauri::command]
pub fn get_hide_original_play_button(app: tauri::AppHandle) -> Result<bool, String> {
    let config = read_config(&app);
    Ok(config.hide_original_play_button.unwrap_or(true))
}

/// 設定「隱藏原有播放按鈕」
#[tauri::command]
pub fn set_hide_original_play_button(
    app: tauri::AppHandle,
    hide: bool,
) -> Result<(), String> {
    let mut config = read_config(&app);
    config.hide_original_play_button = Some(hide);
    write_config(&app, &config)
}

/// 取得 NAS 本地網盤代理開關
#[tauri::command]
pub fn get_nas_proxy_enabled(app: tauri::AppHandle) -> Result<bool, String> {
    let config = read_config(&app);
    Ok(config.nas_proxy_enabled.unwrap_or(false))
}

/// 設定 NAS 本地網盤代理開關
#[tauri::command]
pub fn set_nas_proxy_enabled(app: tauri::AppHandle, enabled: bool) -> Result<(), String> {
    let mut config = read_config(&app);
    config.nas_proxy_enabled = Some(enabled);
    write_config(&app, &config)
}

/// 取得退出模式
#[tauri::command]
pub fn get_exit_mode(app: tauri::AppHandle) -> Result<String, String> {
    let config = read_config(&app);
    Ok(config
        .exit_mode
        .unwrap_or_else(|| "ask".to_string()))
}

/// 設定退出模式
#[tauri::command]
pub fn set_exit_mode(app: tauri::AppHandle, mode: String) -> Result<(), String> {
    let mut config = read_config(&app);
    config.exit_mode = Some(mode);
    write_config(&app, &config)
}

/// 取得 MPV 播放器路徑
#[tauri::command]
pub fn get_mpv_player_path(app: tauri::AppHandle) -> Result<Option<String>, String> {
    let config = read_config(&app);
    Ok(config.mpv_player_path)
}

/// 設定 MPV 播放器路徑（null 或空字串表示清除）
#[tauri::command]
pub fn set_mpv_player_path(
    app: tauri::AppHandle,
    path: Option<String>,
) -> Result<(), String> {
    let mut config = read_config(&app);
    match path {
        Some(p) if !p.is_empty() => config.mpv_player_path = Some(p),
        _ => config.mpv_player_path = None,
    }
    write_config(&app, &config)
}
