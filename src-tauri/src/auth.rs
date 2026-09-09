//! 登入認證模組 — FN API 通訊、登入流程、Cookie 恢復
//!
//! 使用 `crate::fn_api` 共用模組進行 FN API 簽名與請求，
//! 僅保留登入相關的業務邏輯。
//!
//! 職責：
//! 1. 登入 API 呼叫
//! 2. 登入後儲存設定 + 歷史記錄
//! 3. Cookie 恢復（透過 webview eval 設定 Trim-MC-token）

use tauri::Manager;

use crate::config;
use crate::fn_api::{fn_request, HttpMethod};

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
/// 流程：若提供訪問碼 → 先向閘道驗證訪問碼（建立閘道會話 Cookie）
/// → 以解析後的 base URL 呼叫 FN API → 儲存設定（訪問碼加密）→
/// 儲存歷史記錄 → 回傳結果
#[tauri::command]
pub async fn login(
    app: tauri::AppHandle,
    domain: String,
    username: String,
    password: String,
    access_code: Option<String>,
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
    let access_code = access_code.unwrap_or_default();

    // 訪問碼驗證（空訪問碼也會清空歷史授權並直接通過）
    let session = match crate::access_code::verify_access_code(&server, &access_code).await {
        Ok(s) => s,
        Err(e) => {
            log::warn!("訪問碼驗證失敗: {e}");
            return Ok(LoginResult {
                success: false,
                domain: None,
                token: None,
                message: Some(match e {
                    crate::access_code::AccessCodeError::Rejected(msg) => {
                        format!("訪問碼驗證失敗: {msg}")
                    }
                    crate::access_code::AccessCodeError::Network(msg) => {
                        format!("訪問碼驗證失敗: {msg}")
                    }
                }),
            });
        }
    };

    // 訪問碼可能把請求重定向到新端口/地址，登入 API 使用解析後的 base URL
    let server = session.base_url;

    // 呼叫登入 API
    let login_data = serde_json::json!({
        "app_name": "trimemedia-web",
        "username": username,
        "password": password,
    });

    let response = fn_request(&server, "/v/api/v1/login", HttpMethod::Post, "", Some(login_data)).await;

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

            // 儲存設定（domain 使用可能被重定向後的伺服器地址；訪問碼加密儲存）
            config::save_login_config(
                app.clone(),
                username.clone(),
                server.clone(),
                token.clone(),
                Some(access_code.clone()),
                use_https,
            )?;

            // 儲存歷史記錄
            config::add_history(
                app.clone(),
                domain,
                username,
                password,
                Some(access_code),
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

    let mut cookie_js = format!("{}\n{}", token_js, relay_js);

    // 附加訪問碼閘道會話 Cookie（best effort：僅在目前頁面與 grant origin
    // 一致時才會生效；主播放鏈路的閘道 Cookie 已由 Proxy session 註冊攜帶）
    let grant_cookie = crate::access_code::get_access_cookie_header(&domain);
    if !grant_cookie.is_empty() {
        for pair in grant_cookie.split(';') {
            let pair = pair.trim();
            if pair.is_empty() {
                continue;
            }
            cookie_js.push_str(&format!(
                "\ndocument.cookie = '{}; path=/; {secure}samesite={same_site}';",
                escape_js_string(pair),
                secure = secure,
                same_site = same_site,
            ));
        }
    }

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
