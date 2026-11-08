import { build } from 'esbuild';
await build({
  entryPoints: ['packages/transport/index.js'],
  outfile: 'dist/sdk.js',
  bundle: true,
  format: 'iife',
  globalName: 'YooDeskTransport',
  platform: 'browser',
  target: 'chrome120',
  sourcemap: true,
  inject:['./packages/transport/runtime.js'],
  alias: {
    crypto: './packages/transport/browser-crypto.js',
    stream: 'stream-browserify',
    url: 'url/',
    util: 'util/',
  },
});
// 网关交付远程页面适配器；LAN viewer 不加载此 bundle。
await build({
  entryPoints: ['packages/transport/viewer.js'],
  outfile: '../gateway/internal/viewerui/transport.js',
  bundle: true,
  format: 'esm',
  platform: 'browser',
  target: 'chrome120',
  minify: true,
  inject: ['./packages/transport/runtime.js'],
  alias: {
    crypto: './packages/transport/browser-crypto.js',
    stream: 'stream-browserify',
    url: 'url/',
    util: 'util/',
  },
});
