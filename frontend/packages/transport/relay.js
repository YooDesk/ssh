import { YamuxConnection, DC_MAX_FRAME_BODY } from './yamux.js';
import { bindMux } from './channel.js';
import { RemoteHTTP, validateContext } from './http.js';
import { RemoteSocket } from './socket.js';
import { relaySubprotocols } from './protocols.js';

// 原生 WebSocket 没有 bufferedamountlow 事件。仅在缓冲超过水位时采样
// bufferedAmount，确认真实排空后恢复 yamux；不重发帧、不更换业务请求路径。
export async function connectRelay({
  url,
  deviceBind,
  context,
  grant,
  getGrant,
  timeoutMs = 15000,
  signal,
}) {
  validateContext(context);
  const base = new URL(context.origin);
  base.protocol = 'wss:';
  const address = new URL(url, base);
  if (
    address.protocol !== 'wss:' ||
    address.origin.replace('wss:', 'https:') !== new URL(context.origin).origin ||
    address.username ||
    address.password ||
    address.search
  )
    throw new TypeError('trusted relay WSS origin required');
  if (getGrant) {
    grant = await getGrant();
    deviceBind = { ...deviceBind, token: grant };
  }
  signal?.throwIfAborted();
  const ws = new WebSocket(address.href, relaySubprotocols(context.generation));
  ws.binaryType = 'arraybuffer';
  let mux;
  let timer;
  let sampler;
  let completed = false;
  const opened = new Promise((resolve, reject) => {
    const fail = (error) => {
      if (completed) return;
      completed = true;
      signal?.removeEventListener('abort', abort);
      clearTimeout(timer);
      clearTimeout(sampler);
      mux?._close();
      ws.close();
      reject(error);
    };
    const abort = () => fail(signal.reason);
    signal?.addEventListener('abort', abort, { once: true });
    timer = setTimeout(() => fail(new Error('relay binding deadline exceeded')), timeoutMs);
    ws.addEventListener('error', () => fail(new Error('relay connection failed')));
    ws.addEventListener('close', () => {
      mux?._close();
      if (!completed) fail(new Error('relay connection closed'));
    });
    ws.addEventListener(
      'open',
      () => {
        ws.send(JSON.stringify(deviceBind));
        const sample = () => {
          sampler = undefined;
          if (ws.readyState !== WebSocket.OPEN) return;
          if (ws.bufferedAmount <= 32768) mux.resumeWrites();
          if (ws.bufferedAmount > 32768) sampler = setTimeout(sample, 4);
        };
        mux = new YamuxConnection(
          (frame) => {
            if (ws.readyState !== WebSocket.OPEN) return false;
            const limit = 65536 + (frame.length === 12 || frame[1] !== 0 ? 16384 : 0);
            if (ws.bufferedAmount + frame.length > limit) {
              if (sampler === undefined) sampler = setTimeout(sample, 4);
              return false;
            }
            ws.send(frame);
            return true;
          },
          {
            maxFrameBody: DC_MAX_FRAME_BODY,
            autoConsume: false,
            maxStreams: 256,
            maxSendQueue: 262144,
          },
        );
        mux.onClose = () => {
          clearTimeout(sampler);
          ws.close();
        };
        ws.addEventListener('message', (event) => {
          if (!(event.data instanceof ArrayBuffer)) {
            fail(new TypeError('binary relay required'));
            return;
          }
          mux.feed(new Uint8Array(event.data));
        });
        void bindMux(mux, grant)
          .then((leaseVersion) => {
            if (completed) {
              mux._close();
              return;
            }
            completed = true;
            signal?.removeEventListener('abort', abort);
            clearTimeout(timer);
            resolve({
              mux,
              http: new RemoteHTTP(mux, context),
              leaseVersion,
              openSocket(url, protocols) {
                return new RemoteSocket(this.http, url, protocols);
              },
              close() {
                clearTimeout(sampler);
                mux._close();
                ws.close(1000, 'done');
              },
              async selectedPair() {
                return { local: 'websocket', remote: 'websocket' };
              },
            });
          })
          .catch(fail);
      },
      { once: true },
    );
  });
  return opened;
}
