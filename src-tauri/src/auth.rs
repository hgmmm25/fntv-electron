//! 登入認證模組 — FN API 通訊、登入流程、Cookie 恢復
//!
//! 完整移植自 `src/modules/fn_api/request.ts` 的簽名邏輯，
//! 以及 `src/main/handlers/plugins/auth.ts` 的登入流程。
//!
//! 職責：
//! 1. FN API 簽名（Authx header）
//! 2. 登入 API 呼叫（reqwest）
//! 3. 登入後儲存設定 + 歷史記錄
//! 4. Cookie 恢復（透過 webview eval 設定 Trim-MC-token）

use std::time::{SystemTime, UNIX_EPOCH};
use tauri::Manager;

use crate::config;

// ─── FN API 常數 ──────────────────────────────────────────

/// FN API 簽名金鑰
const API_KEY: &str = "NDzZTVxnRKP8Z0jXg1VAMonaG8akvh";
/// FN API 簽名密鑰
const API_SECRET: &str = "16CCEB3D-AB42-077D-36A1-F355324E4237";

// ─── FN API 簽名工具 ──────────────────────────────────────

/// MD5 雜湊（hex 輸出）
fn md5_hex(input: &str) -> String {
    use md5::{Digest, Md5};
    let mut hasher = Md5::new();
    hasher.update(input.as_bytes());
    hex::encode(hasher.finalize())
}

/// 產生隨機數字字串（與 Electron 的 generateRandomDigits 相容）
fn random_digits(start: u64, end: u64) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos() as u64;
    ((nanos % (end - start)) + start).to_string()
}

/// 產生 Authx header（對應 Electron 的 genFnAuthx）
fn gen_fn_authx(url: &str, data: Option<&serde_json::Value>) -> String {
    let nonce = random_digits(100_000, 1_000_000);
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .to_string();

    let data_json = data
        .map(|d| serde_json::to_string(d).unwrap_or_default())
        .unwrap_or_default();
    let data_json_md5 = md5_hex(&data_json);

    let sign_str = format!(
        "{}_{}_{}_{}_{}_{}",
        API_KEY, url, nonce, timestamp, data_json_md5, API_SECRET
    );
    let sign = md5_hex(&sign_str);

    format!("nonce={}&timestamp={}&sign={}", nonce, timestamp, sign)
}

// ─── FN API 通用請求 ──────────────────────────────────────

/// FN API 回應的外層結構（對應 FnApiResponseData）
#[derive(Debug, Clone, serde::Deserialize)]
struct FnApiResponse {
    code: i64,
    msg: String,
    data: Option<serde_json::Value>,
}

/// 通用 FN API 請求（簡化版，僅保留登入所需的功能）
///
/// 對應 `src/modules/fn_api/request.ts` 的 `request()` 函數。
async fn fn_request(
    base_url: &str,
    path: &str,
    token: &str,
    data: Option<serde_json::Value>,
) -> Result<FnApiResponse, String> {
    let full_url = format!("{}{}", base_url, path);

    // POST 請求自動加入 nonce
    let mut request_data = data.clone();
    if let Some(ref mut d) = request_data {
        d.as_object_mut()
            .unwrap()
            .insert("nonce".into(), serde_json::Value::String(random_digits(100_000, 1_000_000)));
    }

    let authx = gen_fn_authx(path, request_data.as_ref());

    let client = reqwest::Client::builder()
        .danger_accept_invalid_certs(true) // 與 Electron 版本一致：信任自簽憑證
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| format!("建立 HTTP client 失敗: {e}"))?;

    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        "Content-Type",
        "application/json"
            .parse()
            .map_err(|_| "Invalid Content-Type header".to_string())?,
    );
    headers.insert(
        "Authorization",
        token
            .parse()
            .map_err(|_| "Invalid Authorization header".to_string())?,
    );
    headers.insert(
        "Cookie",
        "mode=relay"
            .parse()
            .map_err(|_| "Invalid Cookie header".to_string())?,
    );
    headers.insert(
        "Authx",
        authx
            .parse()
            .map_err(|_| "Invalid Authx header".to_string())?,
    );

    // 最多重試 5 次（處理簽名錯誤）
    let max_retries = 5;
    for attempt in 0..=max_retries {
        let resp = client
            .post(&full_url)
            .headers(headers.clone())
            .json(&request_data)
            .send()
            .await
            .map_err(|e| format!("FN API 請求失敗: {e}"))?;

        // 處理重定向
        if resp.status().is_redirection() {
            let location = resp
                .headers()
                .get("location")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .to_string();

            if !location.is_empty() {
                log::warn!("FN API 檢測到重定向 -> {location}");
                // 解析新的 base URL
                if location.starts_with("http") {
                    if let Some(slash_pos) = location[8..].find('/') {
                        let new_base = &location[..8 + slash_pos];
                        let new_path = &location[8 + slash_pos..];
                        return Box::pin(fn_request(
                            new_base,
                            new_path,
                            token,
                            request_data.clone(),
                        ))
                        .await;
                    } else {
                        return Box::pin(fn_request(
                            &location,
                            path,
                            token,
                            request_data.clone(),
                        ))
                        .await;
                    }
                }
            }
        }

        // 非 JSON 回應（如二進制檔案）
        let content_type = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        if !content_type.contains("application/json") {
            return Ok(FnApiResponse {
                code: 0,
                msg: "ok".to_string(),
                data: None,
            });
        }

        let status = resp.status();
        let body: FnApiResponse = resp
            .json()
            .await
            .map_err(|e| format!("FN API 回應解析失敗: {e} (HTTP {status})"))?;

        // 簽名錯誤重試
        if body.code == 5000 && body.msg == "invalid sign" {
            if attempt >= max_retries {
                return Err(format!("FN API 簽名錯誤，重試次數已用盡"));
            }
            log::warn!(
                "FN API 簽名錯誤，重試中 attempt = {}",
                attempt + 1
            );
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            continue;
        }

        // 業務錯誤
        if body.code != 0 {
            return Err(body.msg);
        }

        return Ok(body);
    }

    Err("FN API 請求失敗：超出重試次數".to_string())
}

// ─── Login Result ─────────────────────────────────────────

/// 登入成功結果
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LoginResult {
    pub success: bool,
    pub domain: Option<String>,
    pub token: Option<String>,
    pub message: Option<String>,
}

// ─── Tauri Commands ───────────────────────────────────────

/// 登入命令 — 對應 Electron 的 `handleLogin`
///
/// 流程：呼叫 FN API → 儲存設定 → 儲存歷史記錄 → 回傳結果
#[tauri::command]
pub async fn login(
    app: tauri::AppHandle,
    domain: String,
    username: String,
    password: String,
    use_https: Option<bool>,
) -> Result<LoginResult, String> {
    log::info!("收到登入請求: domain={domain}, username={username}");

    if domain.is_empty() || username.is_empty() || password.is_empty() {
        return Err("請提供完整的登入資訊".to_string());
    }

    // 構建伺服器地址
    let protocol = if use_https.unwrap_or(false) {
        "https"
    } else {
        "http"
    };
    let server = format!("{}://{}", protocol, domain);

    // 呼叫登入 API
    let login_data = serde_json::json!({
        "app_name": "trimemedia-web",
        "username": username,
        "password": password,
    });

    let response = fn_request(&server, "/v/api/v1/login", "", Some(login_data)).await;

    match response {
        Ok(res) => {
            // 取得 token
            let token = res
                .data
                .as_ref()
                .and_then(|d| d.get("token"))
                .and_then(|t| t.as_str())
                .map(|s| s.to_string());

            let token = match token {
                Some(t) if !t.is_empty() => t,
                _ => {
                    return Ok(LoginResult {
                        success: false,
                        domain: None,
                        token: None,
                        message: Some("沒有有效的登入資訊，無法恢復 cookies".to_string()),
                    });
                }
            };

            // 儲存設定（domain 使用可能被重定向後的伺服器地址）
            config::save_login_config(
                app.clone(),
                username.clone(),
                server.clone(),
                token.clone(),
                use_https,
            )?;

            // 儲存歷史記錄
            config::add_history(
                app.clone(),
                domain,
                username,
                password,
                use_https,
            )?;

            log::info!("登入成功，token 長度: {}", token.len());

            Ok(LoginResult {
                success: true,
                domain: Some(server),
                token: Some(token),
                message: None,
            })
        }
        Err(e) => {
            log::error!("登入失敗: {e}");
            Ok(LoginResult {
                success: false,
                domain: None,
                token: None,
                message: Some(format!("登入失敗: {e}")),
            })
        }
    }
}

/// Cookie 恢復命令 — 對應 Electron 的 `restoreCookies`
///
/// 透過 webview 的 eval() 設定 Trim-MC-token 和 mode=relay cookie。
#[tauri::command]
pub async fn restore_cookies(
    app: tauri::AppHandle,
    domain: String,
    token: String,
) -> Result<(), String> {
    if token.is_empty() {
        return Err("沒有已保存的 token".to_string());
    }

    if domain.is_empty() || !domain.starts_with("http") {
        return Err("無效的域名格式".to_string());
    }

    let is_https = domain.starts_with("https://");
    let same_site = if is_https {
        "none"
    } else {
        "lax"
    };
    let secure = if is_https { "secure; " } else { "" };

    // 設定 Trim-MC-token cookie
    let token_js = format!(
        "document.cookie = 'Trim-MC-token={token}; path=/; {secure}samesite={same_site}';",
        token = escape_js_string(&token),
        secure = secure,
        same_site = same_site,
    );

    // 設定 mode=relay cookie
    let relay_js = format!(
        "document.cookie = 'mode=relay; path=/; {secure}samesite={same_site}';",
        secure = secure,
        same_site = same_site,
    );

    let cookie_js = format!("{}\n{}", token_js, relay_js);

    // 透過主視窗 eval 注入 cookie
    if let Some(window) = app.get_webview_window("main") {
        window
            .eval(&cookie_js)
            .map_err(|e| format!("Cookie 設定失敗: {e}"))?;
        log::info!("Cookie 已恢復: domain={domain}");
        Ok(())
    } else {
        Err("找不到主視窗".to_string())
    }
}

/// 輔助函數：轉義 JavaScript 字串中的特殊字元
fn escape_js_string(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('\'', "\\'")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
}
