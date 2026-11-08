import { RemoteSocket } from './socket.js';

const installed = new WeakMap();

// 只路由当前设备范围；范围外调用仍按浏览器原生规则执行。
// 已匹配的业务请求失败直接抛出，禁止重发到原生 fetch/WebSocket。
export function installShim(link, { global = globalThis } = {}) {
  if (installed.has(global)) throw new Error('a device transport shim is already installed');
  const nativeFetch = global.fetch;
  const NativeWebSocket = global.WebSocket;
  const context = link.http.context;
  const isRequest = (input) =>
    input instanceof Request ||
    (typeof global.Request === 'function' && input instanceof global.Request);
  const route = (input) => {
    const url = new URL(isRequest(input) ? input.url : input, context.origin);
    if (url.protocol === 'wss:') url.protocol = 'https:';
    else if (url.protocol === 'ws:') url.protocol = 'http:';
    return (
      url.origin === new URL(context.origin).origin && url.pathname.startsWith(context.pathPrefix)
    );
  };
  const scopedFetch = function (input, init) {
    if (route(input)) {
      // 父页连接服务子 iframe 时，把另一 realm 的 Request 转成连接所属 realm，
      // 保留原生 Request 的 body/headers/signal，不能把对象当成相对 URL 字符串。
      if (isRequest(input) && !(input instanceof Request)) input = new Request(input);
      return link.http.request(input, init);
    }
    return nativeFetch.call(global, input, init);
  };
  function ScopedWebSocket(input, protocols) {
    if (!new.target) throw new TypeError('WebSocket requires new');
    const url = new URL(input, context.origin);
    if (url.protocol === 'https:') url.protocol = 'wss:';
    else if (url.protocol === 'http:') url.protocol = 'ws:';
    if (route(url)) return link.openSocket(url, protocols);
    return protocols === undefined
      ? new NativeWebSocket(input)
      : new NativeWebSocket(input, protocols);
  }
  Object.setPrototypeOf(ScopedWebSocket, NativeWebSocket);
  ScopedWebSocket.prototype = NativeWebSocket.prototype;
  Object.defineProperty(ScopedWebSocket, Symbol.hasInstance, {
    value: (object) => object instanceof RemoteSocket || object instanceof NativeWebSocket,
  });
  global.fetch = scopedFetch;
  global.WebSocket = ScopedWebSocket;
  const remove = () => {
    if (global.fetch === scopedFetch) global.fetch = nativeFetch;
    if (global.WebSocket === ScopedWebSocket) global.WebSocket = NativeWebSocket;
    installed.delete(global);
  };
  installed.set(global, remove);
  return remove;
}
