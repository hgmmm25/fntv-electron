// src-tauri/inject/core/playTarget.ts
// 播放目標（itemGuid）提取工具，移植自上游 src/preload/core/playTarget.ts
// 路由结构优先于 GUID 格式，兼容飞牛影视新旧详情页 / 播放页 URL；
// 供 playButton / playMaskButton 共用。

const GUID_PATTERN = /^(?:[a-z][a-z0-9]*_)?[a-f0-9]{32}$/i;
const UUID_PATTERN = /^[a-f0-9]{8}-[a-f0-9]{4}-[1-5][a-f0-9]{3}-[89ab][a-f0-9]{3}-[a-f0-9]{12}$/i;
const ROUTE_MARKERS = new Set(['movie', 'episode', 'other', 'video', 'detail', 'item', 'play']);
const QUERY_KEYS = ['item_guid', 'itemGuid', 'guid', 'id'];

function safeDecode(value: string): string {
    try {
        return decodeURIComponent(value);
    } catch {
        return value;
    }
}

function normalizeCandidate(value: string | null | undefined): string | null {
    if (!value) return null;

    const candidate = safeDecode(value).trim();
    if (!candidate || candidate.length > 128 || /[/?#&=]/.test(candidate)) return null;
    return candidate;
}

function extractFromPath(path: string): string | null {
    const segments = path
        .split('/')
        .map(normalizeCandidate)
        .filter((segment): segment is string => Boolean(segment));

    // 1) 优先按路由标记定位：<marker>/<id>（如 /v/tv/episode/<guid>、/movie/<id>）
    for (let index = segments.length - 2; index >= 0; index -= 1) {
        if (!ROUTE_MARKERS.has(segments[index].toLowerCase())) continue;
        const candidate = segments[index + 1];
        if (candidate && !ROUTE_MARKERS.has(candidate.toLowerCase())) return candidate;
    }

    // 2) 兜底：扫一遍合法 GUID/UUID 格式
    for (let index = segments.length - 1; index >= 0; index -= 1) {
        const candidate = segments[index];
        if (GUID_PATTERN.test(candidate) || UUID_PATTERN.test(candidate)) return candidate;
    }

    return null;
}

/**
 * 从 URL 中提取播放项标识（兼容 query / path / hash 三种形态）。
 */
export function extractItemGuidFromUrl(value: string): string | null {
    try {
        const url = new URL(value, 'http://fntv.local');

        for (const key of QUERY_KEYS) {
            const candidate = normalizeCandidate(url.searchParams.get(key));
            if (candidate) return candidate;
        }

        const pathCandidate = extractFromPath(url.pathname);
        if (pathCandidate) return pathCandidate;

        const hash = url.hash.replace(/^#/, '');
        if (hash) {
            const hashUrl = new URL(hash.startsWith('/') ? hash : `/${hash}`, 'http://fntv.local');
            for (const key of QUERY_KEYS) {
                const candidate = normalizeCandidate(hashUrl.searchParams.get(key));
                if (candidate) return candidate;
            }
            return extractFromPath(hashUrl.pathname);
        }
    } catch {
        return extractFromPath(value);
    }

    return null;
}

export function isItemGuid(value: string | null | undefined): value is string {
    const candidate = normalizeCandidate(value);
    return candidate !== null && (GUID_PATTERN.test(candidate) || UUID_PATTERN.test(candidate));
}

/**
 * DOM 兜底提取：点击按钮不在详情页 URL 上时，向上找 data-id="details" 容器
 * 并从剧集链接中提取 guid；仍失败则退回 URL 提取。
 */
export function getGuidFromDom(button: HTMLElement): string | null {
    try {
        let container: Element | null = button;
        while (container && container !== document.body) {
            if (container.getAttribute('data-id') === 'details') {
                const aLinks = container.querySelectorAll('a[href*="/v/tv/episode/"]');
                if (aLinks.length > 0) {
                    const link = aLinks[0] as HTMLAnchorElement;
                    const guidMatch = link.href.match(/\/v\/tv\/episode\/([a-f0-9]{32})/i);
                    if (guidMatch && guidMatch[1]) return guidMatch[1];
                }
                break;
            }
            container = container.parentElement;
        }

        const urlGuid = extractItemGuidFromUrl(window.location.href);
        return urlGuid || null;
    } catch {
        return null;
    }
}
