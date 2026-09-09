// src-tauri/inject/plugins/playMaskButton.ts
// 遮罩播放按鈕插件（移植自 src/preload/plugins/playMaskButton.ts）
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
import { getGuidFromDom } from '../core/playTarget';

// ─── 透過攔截請求取得 guid ─────────────────────────────────────────

function tryGetItemGuidFromOriginalLogic(button: HTMLElement): Promise<string | null> {
    return new Promise((resolve) => {
        try {
            const originalFetch = window.fetch;
            const originalXHROpen = XMLHttpRequest.prototype.open;
            const originalXHRSend = XMLHttpRequest.prototype.send;

            let interceptedGuid: string | null = null;
            const timeout = setTimeout(() => {
                window.fetch = originalFetch;
                XMLHttpRequest.prototype.open = originalXHROpen;
                XMLHttpRequest.prototype.send = originalXHRSend;
                resolve(null);
            }, 2000);

            // 拦截 fetch
            window.fetch = function (url: RequestInfo | URL, options?: RequestInit): Promise<Response> {
                logger.info('Intercepted fetch request:', url, options);
                if (typeof url === 'string' && url.includes('/api/v1/play/info') && options && options.body) {
                    try {
                        const body = JSON.parse(options.body as string);
                        if (body.item_guid) {
                            interceptedGuid = body.item_guid;
                            logger.info('Found item_guid in fetch request:', interceptedGuid);
                        }
                    } catch (e) {
                        logger.error('Error parsing fetch body:', e);
                    }
                }
                if (typeof url === 'string' && url.includes('/api/v1/play/info')) {
                    return Promise.resolve({
                        ok: false,
                        status: 200,
                        json: () => Promise.resolve({ success: false, message: 'Intercepted for guid extraction' }),
                    } as Response);
                }
                return originalFetch.apply(this, arguments as unknown as [RequestInfo | URL, RequestInit?]);
            };

            // 拦截 XMLHttpRequest
            XMLHttpRequest.prototype.open = function (method: string, url: string | URL): void {
                (this as unknown as Record<string, unknown>)._url = url;
                return originalXHROpen.apply(this, arguments as unknown as [string, string, boolean]);
            };

            XMLHttpRequest.prototype.send = function (data?: Document | XMLHttpRequestBodyInit | null): void {
                const thisXHR = this as unknown as Record<string, unknown>;
                if (thisXHR._url && typeof thisXHR._url === 'string' && thisXHR._url.includes('/api/v1/play/info') && data) {
                    try {
                        const parsedData = JSON.parse(data as string);
                        if (parsedData.item_guid) {
                            interceptedGuid = parsedData.item_guid;
                            logger.info('Found item_guid in XHR request:', interceptedGuid);
                        }
                    } catch (e) {
                        logger.error('Error parsing XHR data:', e);
                    }
                    setTimeout(() => {
                        if (this.onreadystatechange) {
                            (this as unknown as Record<string, unknown>).readyState = 4;
                            (this as unknown as Record<string, unknown>).status = 404;
                            (this as unknown as Record<string, unknown>).responseText =
                                JSON.stringify({ success: false, message: 'Intercepted for guid extraction' });
                            this.onreadystatechange(new Event('readystatechange'));
                        }
                    }, 100);
                    return;
                }
                return originalXHRSend.apply(this, arguments as unknown as [Document | XMLHttpRequestBodyInit | null | undefined]);
            };

            button.setAttribute('data-allow-original-play', 'true');
            setTimeout(() => {
                const clickEvent = new MouseEvent('click', {
                    view: window,
                    bubbles: true,
                    cancelable: true,
                });
                button.dispatchEvent(clickEvent);

                setTimeout(() => {
                    clearTimeout(timeout);
                    window.fetch = originalFetch;
                    XMLHttpRequest.prototype.open = originalXHROpen;
                    XMLHttpRequest.prototype.send = originalXHRSend;
                    button.removeAttribute('data-allow-original-play');
                    resolve(interceptedGuid);
                }, 1000);
            }, 50);
        } catch (error) {
            logger.error('Error in tryGetItemGuidFromOriginalLogic:', error);
            resolve(null);
        }
    });
}

// ─── MPV 播放 ──────────────────────────────────────────────────────

async function playWithMpv(button: HTMLElement): Promise<void> {
    const domResult = sendPlayEventToMain(button);

    if (!domResult) {
        logger.info('DOM method failed, trying original logic interception...');
        const itemGuid = await tryGetItemGuidFromOriginalLogic(button);

        if (itemGuid) {
            logger.info('Successfully obtained item_guid from original logic:', itemGuid);
            const token = getCookie('Trim-MC-token');
            if (token) {
                playMovie({ id: itemGuid, token, sourceIndex: 0 });
            } else {
                logger.error('No token found');
            }
        } else {
            logger.error('All methods failed to get item_guid');
        }
    } else {
        logger.info('Successfully used DOM method to get item_guid');
    }
}

// ─── 發送播放事件 ─────────────────────────────────────────────────

function sendPlayEventToMain(button: HTMLElement | null = null): string | null {
    let id = '';

    if (button) {
        id = getGuidFromDom(button) || '';
    }

    if (!id) {
        return null;
    }

    const token = getCookie('Trim-MC-token');

    if (id && token) {
        playMovie({ id, token, sourceIndex: 0 });
        return id;
    } else {
        logger.error('Failed to extract ID or token. ID:', id, 'Token:', token);
        return null;
    }
}

// ─── 遮罩按鈕攔截 ─────────────────────────────────────────────────

async function interceptMaskButton(): Promise<void> {
    const config = await getPlayButtonConfig();
    if (!config.hideOriginalPlayButton) {
        // 偏好=网页播放：不接管遮罩按钮，放行页面原生逻辑
        return;
    }

    const playButtons = document.querySelectorAll(
        '.play-mask__btn--play:not([data-mask-intercepted])',
    );

    for (let i = 0; i < playButtons.length; i++) {
        const btn = playButtons[i] as HTMLElement;
        btn.setAttribute('data-mask-intercepted', 'true');

        btn.addEventListener(
            'click',
            async (e: Event) => {
                if (btn.getAttribute('data-allow-original-play') === 'true') return;

                e.preventDefault();
                e.stopPropagation();
                e.stopImmediatePropagation();

                logger.info('Play mask click intercepted, directly playing with MPV');
                await playWithMpv(btn);

                return false;
            },
            true,
        );
    }
}

// ─── 註冊 Hook ─────────────────────────────────────────────────────

registerHook(HookType.OnReady, interceptMaskButton);
registerHook(HookType.OnDomChange, interceptMaskButton);
