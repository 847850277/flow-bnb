import { build } from 'esbuild';
import { mkdir, copyFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
const root = fileURLToPath(new URL('.', import.meta.url));
await mkdir(root + 'dist', { recursive: true });
await build({ absWorkingDir: root, entryPoints: ['app.mjs'], bundle: true, outfile: 'dist/app.js', format: 'esm', platform: 'browser', target: ['es2022'], inject: ['buffer-shim.mjs'], define: { global: 'globalThis' }, minify: true, legalComments: 'eof' });
for (const file of ['index.html', 'style.css', 'icon.svg']) await copyFile(root + file, root + 'dist/' + file);
