// src-tauri/inject/bridge.ts
// Tauri 腳本注入橋接器
//
// 在被注入的遠端網頁環境中，透過 window.__TAURI_INTERNALS__.invoke() 與
// Rust 後端安全地通訊，取代 Electron 的 ipcRenderer.send / ipcRenderer.invoke。
//
// 安全性：
// 1. 所有 invoke 都走 __TAURI_INTERNALS__，Tauri 核心會驗證 capabilities
// 2. 載荷包成 { payload } 單一參數，避免 command 引數展開問題
// 3. invoke 失敗時 reject，呼叫端可自行處理（不吞錯）

declare global {
    interface Window {
        __TAURI_INTERNALS__?: {
            invoke: (cmd: string, args?: Record<string, unknown>) => Promise<unknown>;
        };
    }
}

/** 檢查 Tauri 內部 API 是否可用 */
export function isTauri(): boolean {
    return typeof window !== 'undefined' && typeof window.__TAURI_INTERNALS__?.invoke === 'function';
}

/**
 * fire-and-forget（替代 ipcRenderer.send）
 *
 * @param command Rust 端的 #[tauri::command] 名稱（snake_case）
 * @param data    要傳遞的資料，會包成 { payload: data }
 */
export async function send(command: string, data?: unknown): Promise<void> {
    if (!isTauri()) {
        console.warn(`[bridge] Tauri 不可用，無法 send "${command}"`);
        return;
    }
    try {
        const args = data !== undefined ? { payload: data } : {};
        await window.__TAURI_INTERNALS__!.invoke(command, args);
    } catch (e) {
        console.error(`[bridge] send("${command}") 失敗:`, e);
    }
}

/**
 * request-reply（替代 ipcRenderer.invoke）
 *
 * @param command Rust 端的 #[tauri::command] 名稱
 * @param args    命名引數物件，會直接傳給 invoke
 * @returns       Rust command 的回傳值
 */
export async function invoke<T = unknown>(
    command: string,
    args?: Record<string, unknown>,
): Promise<T> {
    if (!isTauri()) {
        throw new Error(`Tauri 不可用，無法 invoke "${command}"`);
    }
    return window.__TAURI_INTERNALS__!.invoke(command, args ?? {}) as Promise<T>;
}

// ─── 通道抽象 ─────────────────────────────────────────────────────
// 將原本分散在各插件的 ipcRenderer.send / .on 邏輯集中成語意化 API，
// 讓插件不需要知道通道名稱的對應關係。

/** 播放電影（替代 ipcRenderer.send('play-movie', playData)） */
export function playMovie(data: { id: string; token: string; sourceIndex: number }): Promise<void> {
    return send('play_movie', data);
}

/**
 * 視窗控制（替代 ipcRenderer.send('window-minimize' / 'window-maximize' / 'window-close')）
 *
 * 基於安全考量，不直接讓遠端網頁呼叫 Tauri 的 window plugin，
 * 而是走自訂的 Rust command（window_minimize / window_toggle_maximize / window_close），
 * 由 Rust 端統一操作視窗。這樣遠端頁面只能透過我們定義的窄介面控制視窗。
 */
export function windowMinimize(): Promise<void> {
    return send('window_minimize');
}
export function windowMaximize(): Promise<void> {
    return send('window_toggle_maximize');
}
export function windowClose(): Promise<void> {
    return send('window_close');
}

/**
 * 取得播放按鈕配置（替代 get-play-button-config + play-button-config-info 的來回）
 *
 * 原本 Electron 用兩個通道（send 請求 + on 監聽回覆 + setTimeout 超時），
 * Tauri 改為單一 invoke 即可同步取回，更安全也更簡潔。
 * 保留 timeout 語意以維持原行為（2 秒後回退到預設值）。
 */
export async function getPlayButtonConfig(): Promise<{ hideOriginalPlayButton: boolean }> {
    const fallback = { hideOriginalPlayButton: true };
    if (!isTauri()) return fallback;

    try {
        const timeout = new Promise<{ hideOriginalPlayButton: boolean }>((resolve) =>
            setTimeout(() => resolve(fallback), 2000),
        );
        const result = (await Promise.race([
            invoke<{ hideOriginalPlayButton: boolean }>('get_play_button_config'),
            timeout,
        ])) as { hideOriginalPlayButton: boolean } | undefined;
        return result ?? fallback;
    } catch (e) {
        console.error('[bridge] getPlayButtonConfig 失敗，使用預設值:', e);
        return fallback;
    }
}

/** 記錄前端日誌（替代 ipcRenderer.invoke('log-message', level, ...args)） */
export function logMessage(level: string, args: unknown[]): void {
    if (!isTauri()) return;
    invoke('log_message', { level, args: args.map((a) => safeStringify(a)) }).catch((e) => {
        console.error('[bridge] logMessage 失敗:', e);
    });
}

// ─── 設定檔管理 Bridge ──────────────────────────────────────

/**
 * 取得設定資料（config + history）
 *
 * 對應 Electron 的 `ipcRenderer.send('get-config')` + `ipcRenderer.on('config-data', ...)`
 * Tauri 改為單一 invoke 同步回傳。
 */
export function getConfig(): Promise<{ config: Record<string, unknown>; history: Array<Record<string, unknown>> }> {
    return invoke('get_config');
}

/**
 * 儲存登入設定
 *
 * 對應 Electron 的 `saveConfig({ account, domain, token, useHttps })`
 */
export function saveLoginConfig(data: {
    account: string;
    domain: string;
    token: string;
    useHttps?: boolean;
}): Promise<void> {
    return invoke('save_login_config', {
        account: data.account,
        domain: data.domain,
        token: data.token,
        useHttps: data.useHttps,
    });
}

/**
 * 取得歷史記錄
 */
export function getHistory(): Promise<Array<{
    domain: string;
    account: string;
    password: string;
    useHttps?: boolean;
}>> {
    return invoke('get_history');
}

/**
 * 新增歷史記錄
 */
export function addHistory(data: {
    domain: string;
    account: string;
    password: string;
    useHttps?: boolean;
}): Promise<void> {
    return invoke('add_history', {
        domain: data.domain,
        account: data.account,
        password: data.password,
        useHttps: data.useHttps,
    });
}

/**
 * 清空歷史記錄
 */
export function clearHistory(): Promise<void> {
    return invoke('clear_history');
}

/**
 * 刪除單筆歷史記錄
 */
export function deleteHistoryItem(data: { domain: string; account: string }): Promise<boolean> {
    return invoke('delete_history_item', {
        domain: data.domain,
        account: data.account,
    });
}

// ─── 登入認證 Bridge ─────────────────────────────────────────

/**
 * 登入
 *
 * 對應 Electron 的 `ipcRenderer.send('login', loginData)`
 * Tauri 改為 invoke 回傳結果。
 */
export function login(data: {
    domain: string;
    username: string;
    password: string;
    useHttps?: boolean;
}): Promise<{ success: boolean; domain?: string; token?: string; message?: string }> {
    return invoke('login', {
        payload: {
            domain: data.domain,
            username: data.username,
            password: data.password,
            useHttps: data.useHttps,
        },
    });
}

/**
 * 恢復 Cookie
 *
 * 對應 Electron 的 `restoreCookies(domain, token)`
 * 透過 webview eval 設定 Trim-MC-token 和 mode=relay cookie。
 */
export function restoreCookies(domain: string, token: string): Promise<void> {
    return invoke('restore_cookies', { domain, token });
}

// ─── 下載代理設定 Bridge ─────────────────────────────────────

/**
 * 取得下載代理設定
 */
export function getDownloadProxyConfig(): Promise<{ enabled: boolean; proxyUrl: string }> {
    return invoke('get_download_proxy_config');
}

/**
 * 設定下載代理
 */
export function setDownloadProxyConfig(data: {
    enabled?: boolean;
    proxyUrl?: string;
}): Promise<void> {
    return invoke('set_download_proxy_config', {
        enabled: data.enabled,
        proxyUrl: data.proxyUrl,
    });
}

/**
 * 設定「隱藏原有播放按鈕」
 */
export function setHideOriginalPlayButton(hide: boolean): Promise<void> {
    return invoke('set_hide_original_play_button', { hide });
}

function safeStringify(value: unknown): string {
    if (typeof value === 'string') return value;
    try {
        return JSON.stringify(value);
    } catch {
        return String(value);
    }
}
