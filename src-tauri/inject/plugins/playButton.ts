// src-tauri/inject/plugins/playButton.ts
// 播放按鈕插件（移植自 src/preload/plugins/playButton.ts）
//
// 變更：
// - ipcRenderer.send('play-movie', ...) → bridge.playMovie(...)
// - ipcRenderer.send/once('get-play-button-config') → bridge.getPlayButtonConfig()
// - logger 改為 inject logger
// - 移除 Electron 特有 import

import { registerHook, HookType } from '../hooks';
import logger from '../logger';
import { getCookie } from '../utils';
import { playMovie, getPlayButtonConfig } from '../bridge';
import { extractItemGuidFromUrl } from '../core/playTarget';

// ─── 版本源索引偵測 ───────────────────────────────────────────────

/**
 * 取得當前高亮的版本按鈕 Index
 * 掃描版本列表，找到樣式為 primary 的按鈕
 */
function getCurrentSelectedVersionIndex(): number {
    try {
        const buttons = Array.from(
            document.querySelectorAll('button.semi-button.\\!h-9.\\!px-6'),
        );

        if (buttons.length === 0) {
            logger.warn('No version buttons found via selector.');
            return 0;
        }

        const selectedIndex = buttons.findIndex((btn) =>
            btn.classList.contains('semi-button-primary'),
        );

        const result = selectedIndex === -1 ? 0 : selectedIndex;
        logger.info(`Found ${buttons.length} buttons, selected index: ${result}`);
        return result;
    } catch (e) {
        logger.error('Error calculating version index:', e);
        return 0;
    }
}

// ─── 發送播放事件 ─────────────────────────────────────────────────

function sendPlayEventToMain(button: HTMLElement | null = null): string | null {
    // 从当前详情页/播放页 URL 按新旧路由提取 itemGuid
    const id = extractItemGuidFromUrl(window.location.href);

    if (!id) {
        logger.error('Failed to extract ID from DOM or URL');
        return null;
    }

    const token = getCookie('Trim-MC-token');
    const sourceIndex = getCurrentSelectedVersionIndex();

    if (id && token) {
        playMovie({ id, token, sourceIndex });
        return id;
    } else {
        logger.error('Failed to extract ID or token. ID:', id, 'Token:', token);
        return null;
    }
}

// ─── 按鈕偵測 ──────────────────────────────────────────────────────

function findReferenceButton(context: Document | Element = document): HTMLButtonElement | null {
    const PLAY_ICON_PATH =
        'M5.984 18.819V5.18c0-1.739 1.939-2.776 3.386-1.812l10.228 6.82a2.177 2.177 0 010 3.623L9.37 20.63c-1.447.964-3.386-.073-3.386-1.812z';

    const buttonsWithPlayIcon = context.querySelectorAll('button');
    for (let i = 0; i < buttonsWithPlayIcon.length; i++) {
        const button = buttonsWithPlayIcon[i];
        const icon = button.querySelector('svg > path[d^="M5.984"]') as SVGPathElement;
        if (icon && icon.getAttribute('d')?.startsWith(PLAY_ICON_PATH.substring(0, 10))) {
            return button as HTMLButtonElement;
        }

        const classes = button.getAttribute('class') || '';
        if (
            classes.includes('semi-button') &&
            classes.includes('semi-button-primary') &&
            classes.includes('!min-w-[150px]')
        ) {
            return button as HTMLButtonElement;
        }
    }

    return null;
}

// ─── 攔截原始按鈕 ─────────────────────────────────────────────────

function interceptOriginalButton(): void {
    const referenceButton = findReferenceButton();
    if (!referenceButton || referenceButton.hasAttribute('data-mpv-intercepted')) return;

    logger.info('Detected page, intercepting original play button...');
    referenceButton.setAttribute('data-mpv-intercepted', 'true');

    const clickHandler = (e: Event) => {
        e.preventDefault();
        e.stopPropagation();
        e.stopImmediatePropagation();

        logger.info('Original play button intercepted, playing with MPV');
        sendPlayEventToMain(referenceButton);
        return false;
    };

    referenceButton.addEventListener('click', clickHandler, true);
}

// ─── 注入流程 ──────────────────────────────────────────────────────

async function injectCustomPlayBtn(): Promise<void> {
    const config = await getPlayButtonConfig();

    if (config.hideOriginalPlayButton) {
        // 偏好=MPV 播放：拦截详情页原始播放按钮，点击改走 MPV
        interceptOriginalButton();
    } else {
        // 偏好=网页播放：不拦截原始按钮，放行页面原生播放逻辑
        logger.info('MPV 接管已关闭（网页播放模式），放行原始播放按钮');
    }
}

function handlePlayButtonInjection(): void {
    injectCustomPlayBtn().catch((error) => {
        logger.error('Error in injectCustomPlayBtn:', error);
    });
}

// ─── 註冊 Hook ─────────────────────────────────────────────────────

registerHook(HookType.OnReady, handlePlayButtonInjection);
registerHook(HookType.OnDomChange, handlePlayButtonInjection);
