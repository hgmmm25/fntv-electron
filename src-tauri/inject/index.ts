// src-tauri/inject/index.ts
// Tauri 注入腳本入口
//
// 對應 src/preload/index.ts 的職責：
// 1. 暴露 logger 到 window（供遠端網頁內聯腳本使用）
// 2. 建立 window.electronAPI 兼容層（供依賴 Electron preload 的頁面使用）
// 3. 明確載入所有插件（Tauri 無 fs.readdirSync，需靜態 import）
// 4. 在 DOM Ready 時執行 OnReady hooks
// 5. 透過 MutationObserver 監聽 DOM 變化，執行 OnDomChange hooks
//
// 本檔案經 esbuild 打包為單一 IIFE，由 Tauri 的 initialization_script 注入。
//
// ## 關於 @tauri-apps/api 與 withGlobalTauri
//
// 本注入腳本**不依賴** `@tauri-apps/api` 套件——直接使用瀏覽器原生的
// `window.__TAURI_INTERNALS__.invoke()`（由 Tauri webview 注入）。
// 因此不需要在 tauri.conf.json 中啟用 `app.withGlobalTauri: true`，
// 也不需要將 @tauri-apps/api bundle 進打包後的程式碼。
// （tauri.conf.json 中的 `withGlobalTauri: true` 會把 @tauri-apps/api
//  掛到 window.__TAURI__，但本腳本走更低層的 __TAURI_INTERNALS__，更穩定。）

// ─── 載入核心模組 ──────────────────────────────────────────────────
import { HookType, runHooks } from './hooks';
import { setupElectronAPIShim } from './bridge';
import logger from './logger';

// ─── 明確載入所有插件 ──────────────────────────────────────────────
// （Eelectron 版本用 fs.readdirSync 自動掃描 plugins/ 目錄；
//   Tauri 注入環境無 Node.js 檔案系統 API，改為靜態 import）
import './plugins/playButton';
import './plugins/playMaskButton';
import './plugins/titlebar';

// 暴露 logger 到 window（與 Electron 版本行為一致）
declare global {
    interface Window {
        log: typeof logger;
        logger: typeof logger;
    }
}

window.log = logger;
window.logger = logger;

// ─── 建立 window.electronAPI 兼容層 ────────────────────────────────
//
// 必須在插件載入後、DOM Ready 前建立，讓遠端頁面代碼能立即使用 electronAPI。
setupElectronAPIShim();

// ─── DOM 初始化 hook 觸發 ──────────────────────────────────────────

function initInjector(): void {
    window.log = logger;
    window.logger = logger;

    if (document.readyState !== 'loading') {
        runHooks(HookType.OnReady);
        observeDomChanges();
    } else {
        document.addEventListener('DOMContentLoaded', () => {
            runHooks(HookType.OnReady);
            observeDomChanges();
        });
    }
}

function observeDomChanges(): void {
    const observer = new MutationObserver(() => runHooks(HookType.OnDomChange));
    observer.observe(document.body, { childList: true, subtree: true });
}

initInjector();
