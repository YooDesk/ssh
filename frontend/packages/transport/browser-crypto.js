// websocket-driver 仅需 RFC6455 的 SHA-1 握手摘要及随机 masking key。
// 使用已审计库和浏览器 CSPRNG，不实现自定义摘要或随机数生成器。
import { sha1 } from '@noble/hashes/legacy.js';
import { Buffer } from 'buffer';

export function randomBytes(size) {
  if (!Number.isInteger(size) || size < 0 || size > 65536)
    throw new RangeError('random byte count');
  return Buffer.from(globalThis.crypto.getRandomValues(new Uint8Array(size)));
}
export function createHash(algorithm) {
  if (algorithm !== 'sha1') throw new TypeError('unsupported WebSocket handshake digest');
  const hash = sha1.create();
  return {
    update(data) {
      hash.update(typeof data === 'string' ? new TextEncoder().encode(data) : data);
      return this;
    },
    digest(encoding) {
      const bytes = Buffer.from(hash.digest());
      return encoding ? bytes.toString(encoding) : bytes;
    },
  };
}
