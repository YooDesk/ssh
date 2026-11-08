// HTTP/1.1 流适配。使用成熟 parser；请求/响应流均受 yamux 窗口背压。
import { Buffer } from 'buffer';
import { HTTPParser } from 'http-parser-js';

const encoder = new TextEncoder();
const MAX_HEADER = 16384;

export function encodeStreamHeader(header) {
  const body = encoder.encode(JSON.stringify(header));
  if (body.length < 1 || body.length > MAX_HEADER) throw new RangeError('RAX2 header size');
  const wire = new Uint8Array(8 + body.length);
  wire.set(encoder.encode('RAX2'));
  new DataView(wire.buffer).setUint32(4, body.length);
  wire.set(body, 8);
  return wire;
}

export function scopedURL(input, context) {
  const url = new URL(input, context.origin);
  const origin = new URL(context.origin);
  if (
    url.origin !== origin.origin ||
    !['http:', 'https:'].includes(url.protocol) ||
    url.username ||
    url.password ||
    !url.pathname.startsWith(context.pathPrefix)
  ) {
    throw new TypeError('request outside authorized device scope');
  }
  const path = url.pathname;
  if (
    path.includes('%') ||
    path.includes('\\') ||
    path.includes('//') ||
    path.split('/').some((part) => ['.', '..', 'admin', 'internal'].includes(part))
  ) {
    throw new TypeError('forbidden request path');
  }
  return url;
}

export function validateContext(context) {
  if (
    !context ||
    ![context.sessionID, context.capability, context.targetID].every(
      (value) => typeof value === 'string' && value.length > 0 && [...value].length <= 128,
    ) ||
    typeof context.generation !== 'string' ||
    !/^[1-9][0-9]{0,19}$/.test(context.generation) ||
    !/^\/[a-z][a-z0-9-]{0,31}\/webclient\//.test(context.pathPrefix) ||
    !context.pathPrefix.endsWith('/')
  )
    throw new TypeError('invalid remote context');
  const origin = new URL(context.origin);
  if (
    origin.origin !== context.origin ||
    origin.username ||
    origin.password ||
    !['http:', 'https:'].includes(origin.protocol)
  )
    throw new TypeError('invalid remote origin');
}

export class RemoteHTTP {
  constructor(mux, context) {
    if (mux.autoConsume) throw new TypeError('manual receive credit required');
    validateContext(context);
    this.mux = mux;
    this.context = Object.freeze({ ...context });
  }

  header(path) {
    const context = this.context;
    return {
      kind: 'http',
      session_id: context.sessionID,
      negotiation_generation: context.generation,
      capability: context.capability,
      target_id: context.targetID,
      path,
    };
  }

  async request(input, init) {
    const url = scopedURL(input instanceof Request ? input.url : input, this.context);
    const request = new Request(input instanceof Request ? input : url, init);
    request.signal.throwIfAborted();
    if (request.method === 'CONNECT' || request.method === 'TRACE')
      throw new TypeError('unsupported HTTP method');
    const stream = this.mux.openStream();
    const path = url.pathname + url.search;
    const parser = new HTTPParser(HTTPParser.RESPONSE);
    parser.maxHeaderSize = 65536;
    const chunks = [];
    let controller;
    let demand = false;
    let complete = false;
    let failed = false;
    let responseStarted = false;
    let uploadDone = false;
    let bodyBytesInFrame = 0;
    let uploadReader;
    let resolveResponse;
    let rejectResponse;
    const ready = new Promise((resolve, reject) => {
      resolveResponse = resolve;
      rejectResponse = reject;
    });

    const cleanup = () => {
      if (complete && uploadDone && chunks.length === 0)
        request.signal.removeEventListener('abort', abort);
    };
    const pump = () => {
      if (failed || !controller) return;
      if (demand && chunks.length) {
        const chunk = chunks.shift();
        demand = false;
        controller.enqueue(chunk);
        stream.consume(chunk.length);
      }
      if (complete && chunks.length === 0) {
        controller.close();
        controller = null;
        cleanup();
      }
    };
    const fail = (error) => {
      if (failed) return;
      failed = true;
      request.signal.removeEventListener('abort', abort);
      rejectResponse(error);
      if (controller) {
        controller.error(error);
        controller = null;
      }
      chunks.length = 0;
      if (uploadReader) void uploadReader.cancel(error).catch(() => {});
      stream.reset();
    };
    const abort = () => fail(request.signal.reason);
    request.signal.addEventListener('abort', abort, { once: true });

    parser[HTTPParser.kOnHeadersComplete] = (info) => {
      if (info.statusCode >= 100 && info.statusCode < 200) {
        if (info.statusCode === 101) throw new TypeError('HTTP request cannot upgrade');
        return true;
      }
      if (responseStarted) throw new TypeError('multiple HTTP exchanges on one stream');
      responseStarted = true;
      const headers = new Headers();
      for (let i = 0; i < info.headers.length; i += 2) {
        const name = info.headers[i].toLowerCase();
        // 浏览器脚本不能读取后端 HttpOnly Cookie；认证上下文由 Connector 管理。
        if (
          !['connection', 'transfer-encoding', 'keep-alive', 'set-cookie', 'set-cookie2'].includes(
            name,
          )
        ) {
          headers.append(name, info.headers[i + 1]);
        }
      }
      const noBody = request.method === 'HEAD' || [204, 205, 304].includes(info.statusCode);
      let body = noBody
        ? null
        : new ReadableStream(
            {
              start(value) {
                controller = value;
              },
              pull() {
                demand = true;
                pump();
              },
              cancel(reason) {
                fail(reason ?? new DOMException('Response cancelled', 'AbortError'));
              },
            },
            { highWaterMark: 0 },
          );
      const encoding = headers.get('content-encoding');
      if (body && encoding && encoding !== 'identity') {
        if (!['gzip', 'deflate'].includes(encoding))
          throw new TypeError('unsupported content encoding');
        body = body.pipeThrough(new DecompressionStream(encoding));
      }
      const response = new Response(body, {
        status: info.statusCode,
        statusText: info.statusMessage,
        headers,
      });
      Object.defineProperty(response, 'url', { value: url.href });
      resolveResponse(response);
      return noBody;
    };
    parser[HTTPParser.kOnBody] = (bytes, offset, length) => {
      bodyBytesInFrame += length;
      chunks.push(new Uint8Array(bytes.subarray(offset, offset + length)));
      pump();
    };
    parser[HTTPParser.kOnMessageComplete] = () => {
      if (!responseStarted) return;
      complete = true;
      pump();
      cleanup();
    };
    stream.onData = (bytes) => {
      if (failed) return;
      try {
        bodyBytesInFrame = 0;
        const consumed = parser.execute(Buffer.from(bytes.buffer, bytes.byteOffset, bytes.length));
        if (consumed instanceof Error) throw consumed;
        if (consumed !== bytes.length) throw new TypeError('unexpected trailing HTTP bytes');
        stream.consume(bytes.length - bodyBytesInFrame);
      } catch (error) {
        fail(error);
      }
    };
    stream.onClose = () => {
      if (failed || complete) return;
      const error = parser.finish();
      if (error instanceof Error) fail(error);
      else if (!complete) fail(new TypeError('HTTP response truncated'));
    };

    // 发送在独立异步任务中推进，收到响应头即可返回，文件不整块缓冲。
    void (async () => {
      await stream.writeAsync(encodeStreamHeader(this.header(path)));
      const headers = new Headers(request.headers);
      for (const name of [
        'host',
        'connection',
        'content-length',
        'transfer-encoding',
        'cookie',
        'accept-encoding',
        'expect',
      ])
        headers.delete(name);
      for (const name of [...headers.keys()]) {
        if (name.startsWith('x-raccess-') || name.startsWith('proxy-')) headers.delete(name);
      }
      headers.set('host', 'backend.invalid');
      headers.set('connection', 'close');
      headers.set('accept-encoding', 'identity');
      if (request.body) headers.set('transfer-encoding', 'chunked');
      let head = `${request.method} ${path} HTTP/1.1\r\n`;
      for (const [name, value] of headers) head += `${name}: ${value}\r\n`;
      head += '\r\n';
      const headBytes = encoder.encode(head);
      if (headBytes.length > 65536) throw new RangeError('HTTP request headers too large');
      await stream.writeAsync(headBytes);
      if (request.body) {
        uploadReader = request.body.getReader();
        while (!failed) {
          const { value, done } = await uploadReader.read();
          if (done) break;
          for (let offset = 0; offset < value.length; offset += 16384) {
            request.signal.throwIfAborted();
            const chunk = value.subarray(offset, offset + 16384);
            await stream.writeAsync(encoder.encode(chunk.length.toString(16) + '\r\n'));
            await stream.writeAsync(chunk);
            await stream.writeAsync(encoder.encode('\r\n'));
          }
        }
        if (!failed) await stream.writeAsync(encoder.encode('0\r\n\r\n'));
      }
      if (!failed) stream.closeWrite();
      uploadDone = true;
      cleanup();
    })().catch(fail);
    return ready;
  }
}
