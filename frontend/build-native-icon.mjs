import { readFile, writeFile } from 'node:fs/promises';
import { createRequire } from 'node:module';
import { extname } from 'node:path';
import { initWasm, Resvg } from '@resvg/resvg-wasm';

const [source, destination] = process.argv.slice(2);
if (!source || !destination) throw new Error('usage: node build-native-icon.mjs <logo> <icon.ico>');
const data = await readFile(source);
const extension = extname(source);
if (extension === '.ico') {
  await writeFile(destination, data);
} else {
  const require = createRequire(import.meta.url);
  await initWasm(await readFile(require.resolve('@resvg/resvg-wasm/index_bg.wasm')));
  const svg = extension === '.png'
    ? `<svg xmlns="http://www.w3.org/2000/svg" width="256" height="256"><image width="256" height="256" href="data:image/png;base64,${data.toString('base64')}"/></svg>`
    : data.toString('utf8');
  const sizes = [16, 24, 32, 48, 64, 128, 256];
  const original = new Resvg(svg);
  const fit = original.width >= original.height ? 'width' : 'height';
  original.free();
  const directory = Buffer.alloc(6 + sizes.length * 16);
  directory.writeUInt16LE(1, 2);
  directory.writeUInt16LE(sizes.length, 4);
  let offset = directory.length;
  const images = sizes.map((size, index) => {
    const image = new Resvg(svg, { fitTo: { mode: fit, value: size } });
    const rendered = image.render();
    // DIB frames support installers that do not accept PNG-compressed small icons.
    const height = size;
    const maskStride = Math.ceil(size / 32) * 4;
    const bitmap = Buffer.alloc(40 + size * height * 4 + maskStride * height);
    bitmap.writeUInt32LE(40);
    bitmap.writeInt32LE(size, 4);
    bitmap.writeInt32LE(height * 2, 8);
    bitmap.writeUInt16LE(1, 12);
    bitmap.writeUInt16LE(32, 14);
    bitmap.writeUInt32LE(size * height * 4, 20);
    const pixels = rendered.pixels;
    const left = Math.floor((size - rendered.width) / 2);
    const top = Math.floor((size - rendered.height) / 2);
    const mask = 40 + size * height * 4;
    bitmap.fill(0xff, mask);
    for (let y = 0; y < rendered.height; y++) {
      for (let x = 0; x < rendered.width; x++) {
        const input = (y * rendered.width + x) * 4;
        const row = height - y - top - 1;
        const col = x + left;
        const output = 40 + (row * size + col) * 4;
        bitmap[output] = pixels[input + 2];
        bitmap[output + 1] = pixels[input + 1];
        bitmap[output + 2] = pixels[input];
        bitmap[output + 3] = pixels[input + 3];
        if (pixels[input + 3])
          bitmap[mask + row * maskStride + (col >> 3)] &= ~(0x80 >> (col & 7));
      }
    }
    const item = 6 + index * 16;
    directory[item] = size % 256;
    directory[item + 1] = height % 256;
    directory.writeUInt16LE(1, item + 4);
    directory.writeUInt16LE(32, item + 6);
    directory.writeUInt32LE(bitmap.length, item + 8);
    directory.writeUInt32LE(offset, item + 12);
    offset += bitmap.length;
    rendered.free();
    image.free();
    return bitmap;
  });
  await writeFile(destination, Buffer.concat([directory, ...images]));
}
