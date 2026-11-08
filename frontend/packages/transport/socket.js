import Client from 'websocket-driver/lib/websocket/driver/client.js';
import { Buffer } from 'buffer';
import { encodeStreamHeader, scopedURL } from './http.js';

const encoder = new TextEncoder();
const MAX_QUEUE = 4 * 1024 * 1024;

// RFC6455 帧、masking、分片、ping/pong/close 均由 websocket-driver 处理。
export class RemoteSocket extends EventTarget {
  static CONNECTING = 0;
  static OPEN = 1;
  static CLOSING = 2;
  static CLOSED = 3;
  CONNECTING = 0;
  OPEN = 1;
  CLOSING = 2;
  CLOSED = 3;
  binaryType = 'blob';
  readyState = 0;
  protocol = '';
  extensions = '';
  _handlers = new Map();
  _wire = [];
  _wireBytes = 0;
  _applicationBytes = 0;
  _sending = false;
  _finished = false;
  _failed = false;
  _sendChain = Promise.resolve();

  constructor(http, input, protocols = []) {
    super();
    const address = new URL(input, http.context.origin);
    if (!['ws:', 'wss:'].includes(address.protocol))
      throw new SyntaxError('WebSocket URL required');
    const requestURL = new URL(address);
    requestURL.protocol = address.protocol === 'wss:' ? 'https:' : 'http:';
    scopedURL(requestURL, http.context);
    this.url = address.href;
    this._stream = http.mux.openStream();
    const driver = (this._driver = new Client(address.href, {
      masking: true,
      protocols,
      maxLength: 2 * 1024 * 1024,
    }));
    driver.setHeader('Origin', new URL(http.context.origin).origin);
    this._header = this._stream.writeAsync(
      encodeStreamHeader(http.header(address.pathname + address.search)),
    );
    driver.io.on('data', (bytes) => {
      if (this._wireBytes + bytes.length > MAX_QUEUE) {
        this._fail(new RangeError('WebSocket wire queue exceeded'));
        return;
      }
      this._wire.push(bytes);
      this._wireBytes += bytes.length;
      void this._drain();
    });
    driver.on('open', () => {
      this.readyState = 1;
      this.protocol = driver.protocol ?? '';
      this.dispatchEvent(new Event('open'));
    });
    driver.on('error', (error) => this._fail(error));
    driver.on('close', (event) => {
      this._finish(event.code, event.reason, !this._failed);
      void this._drain();
    });
    driver.messages.on('data', (message) => {
      let data = message;
      if (typeof message !== 'string') {
        const bytes = new Uint8Array(message);
        data = this.binaryType === 'arraybuffer' ? bytes.buffer : new Blob([bytes]);
      }
      this.dispatchEvent(new MessageEvent('message', { data, origin: address.origin }));
    });
    this._stream.onData = (bytes) => {
      try {
        driver.io.write(Buffer.from(bytes.buffer, bytes.byteOffset, bytes.length));
        this._stream.consume(bytes.length);
      } catch (error) {
        this._fail(error);
      }
    };
    this._stream.onClose = () => {
      driver.io.end();
      if (!this._finished) this._finish(1006, '', false);
    };
    void this._header
      .then(() => {
        if (this.readyState === 0) driver.start();
      })
      .catch((error) => this._fail(error));
  }

  get bufferedAmount() {
    return this._applicationBytes + this._wireBytes;
  }

  send(input) {
    if (this.readyState === 0)
      throw new DOMException('WebSocket is connecting', 'InvalidStateError');
    if (this.readyState !== 1) return;
    let data = input;
    if (!(
      typeof data === 'string' ||
      data instanceof Blob ||
      data instanceof ArrayBuffer ||
      ArrayBuffer.isView(data)
    ))
      data = String(data);
    const size =
      typeof data === 'string'
        ? encoder.encode(data).length
        : data instanceof Blob
          ? data.size
          : data.byteLength;
    if (size > 2 * 1024 * 1024 || this.bufferedAmount + size > MAX_QUEUE)
      throw new RangeError('WebSocket message or queue limit exceeded');
    // 与原生 WebSocket 一致，在 send 返回前复制可变二进制数据。
    // noVNC 会立即复用其发送缓冲，不能等异步队列执行时才读取原视图。
    if (typeof data !== 'string' && !(data instanceof Blob)) {
      data = new Uint8Array(
        data instanceof ArrayBuffer ? data : data.buffer,
        data.byteOffset ?? 0,
        data.byteLength,
      ).slice();
    }
    this._applicationBytes += size;
    this._sendChain = this._sendChain
      .then(async () => {
        if (data instanceof Blob) data = await data.arrayBuffer();
        this._applicationBytes -= size;
        if (this._finished) return;
        if (typeof data === 'string') this._driver.text(data);
        else
          this._driver.binary(
            Buffer.from(
              data instanceof ArrayBuffer ? data : data.buffer,
              data.byteOffset ?? 0,
              data.byteLength,
            ),
          );
      })
      .catch((error) => this._fail(error));
  }

  close(code = 1000, reason = '') {
    if (code !== 1000 && !(code >= 3000 && code <= 4999))
      throw new DOMException('Invalid close code', 'InvalidAccessError');
    if (encoder.encode(reason).length > 123) throw new SyntaxError('close reason too long');
    if (this.readyState >= 2) return;
    if (this.readyState === 0) {
      this._fail(new Error('closed before handshake'));
      return;
    }
    this.readyState = 2;
    void this._sendChain
      .then(() => this._driver.close(reason, code))
      .catch((error) => this._fail(error));
    this._closeTimer = setTimeout(() => {
      if (!this._finished) {
        this._stream.reset();
        this._finish(1006, '', false);
      }
    }, 5000);
  }

  async _drain() {
    if (this._sending) return;
    this._sending = true;
    try {
      await this._header;
      while (this._wire.length) {
        const bytes = this._wire[0];
        for (let offset = 0; offset < bytes.length; offset += 16384) {
          const chunk = bytes.subarray(offset, offset + 16384);
          await this._stream.writeAsync(chunk);
          this._wireBytes -= chunk.length;
        }
        this._wire.shift();
      }
      if (this._finished) this._stream.closeWrite();
    } catch (error) {
      this._fail(error);
    } finally {
      this._sending = false;
    }
  }

  _fail(_error) {
    if (this._finished) return;
    this._failed = true;
    this.dispatchEvent(new Event('error'));
    this._finish(1006, '', false);
    this._stream.reset();
  }
  _finish(code, reason, wasClean) {
    if (this._finished) return;
    this._finished = true;
    this.readyState = 3;
    clearTimeout(this._closeTimer);
    this.dispatchEvent(new CloseEvent('close', { code, reason, wasClean }));
  }
}

for (const name of ['open', 'message', 'error', 'close'])
  Object.defineProperty(RemoteSocket.prototype, 'on' + name, {
    get() {
      return this._handlers.get(name) ?? null;
    },
    set(handler) {
      const previous = this._handlers.get(name);
      if (previous) this.removeEventListener(name, previous);
      this._handlers.delete(name);
      if (typeof handler === 'function') {
        this._handlers.set(name, handler);
        this.addEventListener(name, handler);
      }
    },
  });
