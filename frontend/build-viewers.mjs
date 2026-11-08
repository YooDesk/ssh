// 成熟协议组件只打包算法；产品页面和工具栏由 WebService 提供。
import { build } from 'esbuild';
import { mkdir, copyFile } from 'node:fs/promises';
import { viewerAdapters } from './patch-viewers.mjs';
const outdir = '../web-service/assets/viewers';
await mkdir(outdir, { recursive: true });
await build({
  entryPoints: [
    'packages/viewer/vnc.js',
    'packages/viewer/ssh.js',
    'packages/viewer/rdp.js',
    'packages/viewer/desktop-workspace.js',
    'packages/viewer/recording.js',
    'packages/viewer/recording-writer.js',
  ],
  outdir,
  bundle: true,
  format: 'esm',
  platform: 'browser',
  target: 'chrome120',
  minify: true,
  legalComments: 'linked',
  plugins: [viewerAdapters],
});
await copyFile('node_modules/@novnc/novnc/LICENSE.txt', `${outdir}/noVNC-LICENSE.txt`);
await copyFile('node_modules/@xterm/xterm/css/xterm.css', `${outdir}/xterm.css`);
await copyFile('node_modules/@xterm/xterm/LICENSE', `${outdir}/xterm-LICENSE.txt`);
