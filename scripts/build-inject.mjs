// scripts/build-inject.mjs
// 將 src-tauri/inject/ 下的 TypeScript 注入腳本打包為單一 IIFE JS 檔案，
// 供 Tauri 透過 initialization_script() 注入到遠端網頁。
//
// 使用：node scripts/build-inject.mjs
// 對應 package.json 的 "build:inject" script。

import esbuild from 'esbuild';
import { fileURLToPath } from 'node:url';
import { dirname, resolve } from 'node:path';

const __dirname = dirname(fileURLToPath(import.meta.url));
const projectRoot = resolve(__dirname, '..');

const entry = resolve(projectRoot, 'src-tauri/inject/index.ts');
const outfile = resolve(projectRoot, 'src-tauri/inject/preload.iife.js');

try {
    await esbuild.build({
        entryPoints: [entry],
        bundle: true,
        minify: true,
        format: 'iife',
        target: 'es2022',
        platform: 'browser',
        outfile,
        logLevel: 'info',
        // 注入腳本可能在任何網頁執行，避免污染全域；命名空間隔離由各模組自行處理
        // process.env.NODE_ENV 替換，移除 Node 專用程式碼分支
        define: {
            'process.env.NODE_ENV': '"production"',
        },
    });

    console.log(`✅  注入腳本已打包: ${outfile}`);
} catch (err) {
    console.error('❌  打包注入腳本失敗:', err);
    process.exit(1);
}
