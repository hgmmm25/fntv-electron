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

// ─── DOM guid 偵測 ────────────────────────────────────────────────

function getItemGuidFromDOM(button: HTMLElement): string | null {
    try {
        let container: Element | null = button;
        while (container && container !== document.body) {
            if (container.getAttribute('data-id') === 'details') {
                const aLinks = container.querySelectorAll('a[href*="/v/tv/episode/"]');
                if (aLinks.length > 0) {
                    const link = aLinks[0] as HTMLAnchorElement;
                    const guidMatch = link.href.match(/\/v\/tv\/episode\/([a-f0-9]{32})/i);
                    if (guidMatch && guidMatch[1]) {
                        logger.info('Found guid:', guidMatch[1]);
                        return guidMatch[1];
                    }
                }
                break;
            }
            container = container.parentElement;
        }

        const url = window.location.href;
        const urlMatch = url.match(/\/v\/tv\/episode\/([a-f0-9]{32})/i);
        if (urlMatch && urlMatch[1]) {
            logger.info('Found guid from URL:', urlMatch[1]);
            return urlMatch[1];
        }

        return null;
    } catch (error) {
        logger.error('Error extracting guid from DOM:', error);
        return null;
    }
}

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
        id = getItemGuidFromDOM(button) || '';
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

// ─── 選擇彈窗 ─────────────────────────────────────────────────────

function createPlayModal(originalButton: HTMLElement): void {
    const existingModal = document.getElementById('play-choice-modal');
    if (existingModal) existingModal.remove();

    // 彈窗遮罩
    const modalOverlay = document.createElement('div');
    modalOverlay.id = 'play-choice-modal';
    modalOverlay.style.cssText = `
        position:fixed;top:0;left:0;width:100%;height:100%;
        background-color:rgba(0,0,0,0.3);z-index:10000;
        display:flex;justify-content:center;align-items:center;
        backdrop-filter:blur(15px);-webkit-backdrop-filter:blur(15px);
    `;

    // 內容容器
    const modalContent = document.createElement('div');
    modalContent.style.cssText = `
        background:rgba(255,255,255,0.1);border-radius:20px;padding:32px;min-width:380px;
        box-shadow:0 8px 32px rgba(0,0,0,0.3),inset 0 1px 0 rgba(255,255,255,0.2),inset 0 -1px 0 rgba(0,0,0,0.1);
        border:1px solid rgba(255,255,255,0.18);
        backdrop-filter:blur(20px);-webkit-backdrop-filter:blur(20px);
    `;

    const title = document.createElement('h3');
    title.textContent = '选择播放方式';
    title.style.cssText = `
        margin:0 0 24px 0;font-size:20px;font-weight:600;text-align:center;
        color:#ffffff;text-shadow:0 2px 4px rgba(0,0,0,0.3);letter-spacing:0.5px;
    `;

    const buttonContainer = document.createElement('div');
    buttonContainer.style.cssText = 'display:flex;gap:16px;justify-content:center;flex-wrap:wrap;';

    const makeBtn = (
        text: string,
        normalBg: string,
        normalBorder: string,
        hoverBg: string,
        hoverBorder: string,
    ): HTMLButtonElement => {
        const btn = document.createElement('button');
        btn.textContent = text;
        btn.style.cssText = `
            padding:12px 24px;background:${normalBg};border:1px solid ${normalBorder};
            border-radius:12px;color:white;cursor:pointer;font-size:14px;font-weight:500;
            transition:all 0.3s ease;min-width:100px;
            backdrop-filter:blur(10px);-webkit-backdrop-filter:blur(10px);
            box-shadow:0 4px 15px rgba(0,0,0,0.15);
        `;
        btn.addEventListener('mouseenter', () => {
            btn.style.background = hoverBg;
            btn.style.borderColor = hoverBorder;
            btn.style.transform = 'translateY(-3px)';
        });
        btn.addEventListener('mouseleave', () => {
            btn.style.background = normalBg;
            btn.style.borderColor = normalBorder;
            btn.style.transform = 'translateY(0)';
        });
        return btn;
    };

    const originalPlayBtn = makeBtn(
        '原有播放',
        'rgba(255,255,255,0.15)',
        'rgba(255,255,255,0.3)',
        'rgba(255,255,255,0.25)',
        'rgba(255,255,255,0.5)',
    );

    const mpvPlayBtn = makeBtn(
        'MPV播放',
        'rgba(102,126,234,0.8)',
        'rgba(102,126,234,0.6)',
        'rgba(102,126,234,0.9)',
        'rgba(102,126,234,0.8)',
    );

    const cancelBtn = makeBtn(
        '取消',
        'rgba(255,255,255,0.1)',
        'rgba(255,255,255,0.2)',
        'rgba(255,255,255,0.2)',
        'rgba(255,255,255,0.4)',
    );

    originalPlayBtn.addEventListener('click', () => {
        modalOverlay.remove();
        logger.info('用户选择了原有播放');
        if (originalButton) {
            originalButton.setAttribute('data-allow-original-play', 'true');
            setTimeout(() => {
                originalButton.dispatchEvent(new MouseEvent('click', { view: window, bubbles: true, cancelable: true }));
                setTimeout(() => originalButton.removeAttribute('data-allow-original-play'), 1000);
            }, 50);
        }
    });

    mpvPlayBtn.addEventListener('click', async () => {
        modalOverlay.remove();
        logger.info('用户选择了MPV播放');
        await playWithMpv(originalButton);
    });

    cancelBtn.addEventListener('click', () => modalOverlay.remove());
    modalOverlay.addEventListener('click', (e: MouseEvent) => {
        if (e.target === modalOverlay) modalOverlay.remove();
    });

    const escHandler = (e: KeyboardEvent) => {
        if (e.key === 'Escape') {
            modalOverlay.remove();
            document.removeEventListener('keydown', escHandler);
        }
    };
    document.addEventListener('keydown', escHandler);

    buttonContainer.append(originalPlayBtn, mpvPlayBtn, cancelBtn);
    modalContent.append(title, buttonContainer);
    modalOverlay.appendChild(modalContent);
    document.body.appendChild(modalOverlay);
}

// ─── 遮罩按鈕攔截 ─────────────────────────────────────────────────

async function interceptMaskButton(): Promise<void> {
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

                const config = await getPlayButtonConfig();

                if (config.hideOriginalPlayButton) {
                    logger.info('Play button click intercepted, directly playing with MPV');
                    await playWithMpv(btn);
                } else {
                    logger.info('Play button click intercepted, showing modal');
                    createPlayModal(btn);
                }

                return false;
            },
            true,
        );
    }
}

// ─── 註冊 Hook ─────────────────────────────────────────────────────

registerHook(HookType.OnReady, interceptMaskButton);
registerHook(HookType.OnDomChange, interceptMaskButton);
