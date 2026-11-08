import { createRequire } from 'node:module';
const { build }=createRequire(new URL('../frontend/package.json',import.meta.url))('esbuild');
import { mkdir, copyFile, cp } from 'node:fs/promises';
import { viewerAdapters } from '../frontend/patch-viewers.mjs';
const kind=process.argv[2];
await mkdir('browser',{recursive:true});
if (kind!=='desk') {
  const entries=kind==='webservice' ? ['desktop-workspace','recording','recording-writer'] : [kind];
  await build({entryPoints:entries.map(name=>`frontend/packages/viewer/${name}.js`),outdir:'browser',bundle:true,format:'esm',platform:'browser',target:'chrome120',minify:true,legalComments:'linked',plugins:[viewerAdapters]});
}
if (kind==='ssh') {
  await copyFile('frontend/node_modules/@xterm/xterm/css/xterm.css','browser/xterm.css');
  await copyFile('frontend/node_modules/@xterm/xterm/LICENSE','browser/xterm-LICENSE.txt');
}
if (kind==='vnc') await copyFile('frontend/node_modules/@novnc/novnc/LICENSE.txt','browser/noVNC-LICENSE.txt');
if (kind==='desk') await cp('browser_static','browser',{recursive:true});
if (kind==='rdp') await cp('assets/ironrdp','browser/ironrdp',{recursive:true});
