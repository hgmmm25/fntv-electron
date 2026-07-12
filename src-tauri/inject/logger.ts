// src-tauri/inject/logger.ts
// 渲染進程日誌模組（移植自 src/preload/core/logger.ts，改走 Tauri bridge）

import { logMessage } from './bridge';

export interface Logger {
    debug: (...args: unknown[]) => void;
    info: (...args: unknown[]) => void;
    warn: (...args: unknown[]) => void;
    error: (...args: unknown[]) => void;
    log: (...args: unknown[]) => void;
    d: (...args: unknown[]) => void;
    i: (...args: unknown[]) => void;
    w: (...args: unknown[]) => void;
    e: (...args: unknown[]) => void;
}

const logger: Logger = {
    debug: (...args: unknown[]): void => logMessage('debug', args),
    info: (...args: unknown[]) => logMessage('info', args),
    warn: (...args: unknown[]) => logMessage('warn', args),
    error: (...args: unknown[]) => logMessage('error', args),
    log: (...args: unknown[]) => logMessage('info', args),
    d: (...args: unknown[]) => logger.debug(...args),
    i: (...args: unknown[]) => logger.info(...args),
    w: (...args: unknown[]) => logger.warn(...args),
    e: (...args: unknown[]) => logger.error(...args),
};

export default logger;
