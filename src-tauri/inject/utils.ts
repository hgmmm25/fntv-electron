// src-tauri/inject/utils.ts
// 純 DOM 工具函數（移植自 src/preload/core/utils.ts，無外部依賴）

/** 檢查當前頁面是否為最終層（電影 / 劇集 / 其他） */
export function checkFinalPageUrl(): boolean {
    const url = window.location.href;
    return url.includes('/v/movie/') || url.includes('/v/tv/episode/') || url.includes('/v/other/');
}

/** 檢查當前頁面是否為季頁面 */
export function checkSeasonPageUrl(): boolean {
    return window.location.href.includes('/v/tv/season/');
}

/** 檢查當前頁面是否為劇集頁面 */
export function checkTVPageUrl(): boolean {
    return window.location.href.includes('/v/tv/');
}

/** 從 document.cookie 取得指定名稱的值 */
export function getCookie(name: string): string | null {
    const cookies = document.cookie.split(';');
    const nameEQ = name + '=';

    for (const cookie of cookies) {
        const trimmed = cookie.trim();
        if (trimmed.startsWith(nameEQ)) {
            return trimmed.substring(nameEQ.length);
        }
    }
    return null;
}
