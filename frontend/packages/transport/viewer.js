import { connectPreferredControl } from './preferred-control.js';
import { NegotiationInterrupted, negotiateSession } from './negotiation.js';
import { releaseSession } from './release.js';
import { maintainSessionLease } from './lease.js';
import { signalSubprotocols } from './protocols.js';
import { installShim } from './shim.js';

export async function attachViewer(productID) {
  let owner = window;
  // 外部 iframe 父页不具备本设备身份；跨源父页不可读取，也不阻止本页独立授权。
  try {
    if (
      window.parent !== window &&
      window.parent.location.origin === location.origin &&
      window.parent.location.pathname.startsWith(`/${productID}/webclient/`) &&
      window.parent.__yoodeskTransportReady
    )
      owner = window.parent;
  } catch (error) {
    if (error.name !== 'SecurityError') throw error;
  }
  owner.__yoodeskTransportReady ||= owner === window && location.pathname.endsWith('/desk/')
    ? connectDeskEntry(productID)
    : connectViewer(productID);
  const transport = await owner.__yoodeskTransportReady;
  if (transport.productID !== productID || transport.origin !== location.origin)
    throw new Error('viewer transport identity changed');
  transport.attach(window);
  return transport.state;
}

async function refreshDeskEntry(scope, signal) {
  const entry = new URL(location.pathname, location.origin);
  entry.searchParams.set('device_id', scope.deviceID);
  if (scope.path) entry.searchParams.set('transport', scope.path);
  const timeout = AbortSignal.timeout(8000);
  const response = await scope.nativeFetch(entry, {
    credentials: 'same-origin', cache: 'no-store',
    signal: signal ? AbortSignal.any([signal, timeout]) : timeout,
  });
  if (!response.ok || new URL(response.url).origin !== entry.origin ||
      !(response.headers.get('content-type') || '').includes('text/html')) {
    const error = new Error(`viewer recovery entry unavailable (${response.status})`);
    error.status = response.status;
    throw error;
  }
  await response.body?.cancel().catch(() => {});
}

function waitForRetry(delay, signal) {
  return new Promise((resolve, reject) => {
    const abort = () => { clearTimeout(timer); reject(signal.reason); };
    const timer = setTimeout(() => { signal.removeEventListener('abort', abort); resolve(); }, delay);
    if (signal.aborted) abort();
    else signal.addEventListener('abort', abort, { once: true });
  });
}

async function connectDeskEntry(productID) {
  const controller = new AbortController();
  const leaving = () => controller.abort(new DOMException('Viewer closed', 'AbortError'));
  addEventListener('pagehide', leaving, { once: true });
  let scope;
  const delays = [1000, 2000, 5000, 10000, 15000];
  try {
    for (let attempt = 0; ; attempt++) {
      controller.signal.throwIfAborted();
      if (scope) {
        await waitForRetry(delays[Math.min(attempt - 1, delays.length - 1)], controller.signal);
        try {
          await refreshDeskEntry(scope, controller.signal);
        } catch (error) {
          if (attempt >= 10 || [401, 403, 410].includes(error.status)) throw error;
          continue;
        }
      }
      try {
        const transport = await connectViewer(productID, scope);
        if (controller.signal.aborted) {
          await transport.close();
          controller.signal.throwIfAborted();
        }
        return transport;
      } catch (error) {
        if (!(error instanceof NegotiationInterrupted) || !error.deskScope || attempt >= 10) throw error;
        scope ||= error.deskScope;
      }
    }
  } finally {
    removeEventListener('pagehide', leaving);
  }
}

// 一个顶层页面拥有一个业务连接，协议工作区的同源子窗口只安装各自可卸载的 shim。
// 导航/静态资源仍由浏览器加载；只有当前产品 webclient 范围内的业务流进入 SDK。
export async function connectViewer(productID, recovery) {
  if (!/^[a-z][a-z0-9-]{0,31}$/.test(productID)) throw new TypeError('invalid product');
  const origin = location.origin;
  const requestedTransport = new URLSearchParams(location.search).get('transport');
  const requestedPath =
    requestedTransport === 'wss' ? 'websocket' : requestedTransport || undefined;
  if (requestedPath !== undefined && !['stun', 'turn', 'websocket'].includes(requestedPath))
    throw new TypeError('invalid viewer transport');
  const nativeFetch = recovery?.nativeFetch ?? globalThis.fetch.bind(globalThis);
  const NativeWebSocket = recovery?.NativeWebSocket ?? globalThis.WebSocket;
  const endpoint = `/${productID}/transport/session/`;
  const selectionDeadline = performance.now() + 20000;
  const selectionSignal = AbortSignal.timeout(20000);
  const read = async (options) => {
    const response = await nativeFetch(endpoint, {
      credentials: 'same-origin',
      redirect: 'error',
      cache: 'no-store',
      signal: AbortSignal.timeout(25000),
      ...options,
    });
    if (!response.ok)
      throw new Error(`remote transport authorization rejected (${response.status})`);
    const value = await response.json();
    if (value.v !== 2 || !value.data) throw new Error('invalid viewer transport bootstrap');
    return value.data;
  };
  const bootstrap = await read({ signal: selectionSignal });
  const pageHeaders = {
    'X-CSRF-Token': bootstrap.csrf_token,
    'X-YooDesk-Viewer-Page': bootstrap.page_id,
  };
  let authorized, context, common;
  const deskScope = () => ({
    deviceID: authorized.device_id, capability: authorized.capability,
    targetID: authorized.target_id, path: requestedPath, nativeFetch, NativeWebSocket,
  });
  const abort = new AbortController();
  const removers = new Map();
  let link,
    closed = false;
  let leaseMaintenance;
  let pageTimer,
    pageRenewing = false;
  const close = async ({ uninstall = true } = {}) => {
    if (uninstall) {
      for (const remove of removers.values()) remove();
      removers.clear();
    }
    if (closed) return;
    closed = true;
    clearInterval(pageTimer);
    abort.abort(new DOMException('Viewer closed', 'AbortError'));
    if (link) await link.close();
  };
  const leaving = () => {
    void close().catch(() => {});
  };
  addEventListener('pagehide', leaving, { once: true });
  const failed = async () => {
    for (const frame of removers.keys()) {
      frame.document.documentElement.dataset.transportState = 'closed';
      frame.document.dispatchEvent(new frame.Event('access-expired'));
    }
    // 保留失败的路由，禁止后续业务请求静默返回原生转发。
    await close({ uninstall: false }).catch(() => {});
  };
  const handoffLease = async () => {
    if (leaseMaintenance) {
      leaseMaintenance.handoff.abort();
      authorized = await leaseMaintenance.promise;
      leaseMaintenance = undefined;
    }
  };
  const maintain = (control, session) => {
    clearInterval(pageTimer);
    if (leaseMaintenance?.control === control) return;
    const handoff = new AbortController();
    const promise = maintainSessionLease({
      ...common,
      session,
      control,
      signal: abort.signal,
      handoffSignal: handoff.signal,
      // 正常授权15分钟，仅由当前信令所有者每5分钟维护。
      // 较短的账号授权仍由维护器按真实截止提前处理。
      onUpdate(next) {
        authorized = next;
      },
    });
    leaseMaintenance = { control, handoff, promise };
    void promise.catch(failed);
  };
  // 仅在建连尚未交给信令所有者时维护原页面授权；绑定后与业务共用授权。
  // 最多一个在途请求，主维护器启动后立即撤掉此临时任务。
  pageTimer = setInterval(async () => {
    if (closed || pageRenewing) return;
    pageRenewing = true;
    try {
      const response = await nativeFetch(`/${productID}/transport/keepalive/`, {
        method: 'POST',
        credentials: 'same-origin',
        redirect: 'error',
        cache: 'no-store',
        headers: pageHeaders,
        signal: AbortSignal.any([abort.signal, AbortSignal.timeout(10000)]),
      });
      if (response.status !== 204) throw new Error('viewer resource lease rejected');
    } catch {
      if (!closed) await failed();
    } finally {
      pageRenewing = false;
    }
    // 页面授权也可能被设备/节点剩余租约截短；预留5秒维护间隔，
    // 避免下一次10秒定时请求落在节点已关闭原授权的边界上。
  }, 5000);
  try {
    // 业务创建可能在准入队列中等待；页面已有自己的授权，必须从取得证明时续租。
    const created = await read({
      method: 'POST',
      headers: pageHeaders,
      signal: AbortSignal.any([abort.signal, selectionSignal]),
    });
    if (closed) throw abort.signal.reason;
    authorized = created.session;
    if (recovery && (authorized.device_id !== recovery.deviceID ||
        authorized.capability !== recovery.capability || authorized.target_id !== recovery.targetID)) {
      throw new Error('viewer recovery scope changed');
    }
    const csrfToken = created.csrf_token;
    context = {
      origin,
      pathPrefix: `/${productID}/webclient/`,
      sessionID: authorized.session_id,
      generation: authorized.negotiation_generation,
      capability: authorized.capability,
      targetID: authorized.target_id,
    };
    common = { origin, productID, csrfToken, fetchImpl: nativeFetch };
    link = await connectPreferredControl({
      requestedPath,
      context,
      policy: created.policy,
      timeoutMs: Math.max(1, Math.ceil(selectionDeadline - performance.now())),
      signal: AbortSignal.any([abort.signal, selectionSignal]),
      async prepare(path, budget) {
        await handoffLease();
        budget.signal.throwIfAborted();
        authorized = await negotiateSession({
          ...common,
          session: authorized,
          path,
          requestID: crypto.randomUUID(),
          signal: budget.signal,
          timeoutMs: budget.remaining(),
        });
        const owned = authorized;
        return {
          session: owned,
          connectionGeneration: owned.connection_generation,
          grant: owned.grant,
          async getGrant() {
            await handoffLease();
            return authorized.grant;
          },
          context: { ...context, generation: owned.negotiation_generation },
          configuration: { iceServers: [], iceTransportPolicy: 'all' },
          url: owned.relay_url,
          deviceBind: { type: 'device.bind', token: owned.grant },
          async dispose({ retrying = false } = {}) {
            if (!retrying)
              await releaseSession({ ...common, session: owned, requestID: crypto.randomUUID() });
          },
        };
      },
      onControl(control, approved) {
        maintain(control, approved.session);
      },
      openSignal(approved, budget) {
        return new Promise((resolve, reject) => {
          const socket = new NativeWebSocket(
            approved.session.signal_url,
            signalSubprotocols(approved.session.negotiation_generation),
          );
          const timer = setTimeout(() => fail(), budget.remaining());
          const cleanup = () => {
            clearTimeout(timer);
            budget.signal.removeEventListener('abort', fail);
          };
          const fail = () => {
            cleanup();
            socket.close();
            reject(new Error('remote signaling unavailable'));
          };
          socket.addEventListener(
            'open',
            () => {
              cleanup();
              resolve(socket);
            },
            { once: true },
          );
          socket.addEventListener('error', fail, { once: true });
          budget.signal.addEventListener('abort', fail, { once: true });
          if (budget.signal.aborted) fail();
        });
      },
    });
    if (closed) {
      await link.close();
      throw abort.signal.reason;
    }
    maintain(link.signalControl, authorized);
    const pair = await link.selectedPair();
    const state = Object.freeze({
      path: link.path,
      pair,
      attempts: link.attempts.map((x) => ({ ...x })),
    });
    return {
      productID,
      origin,
      state,
      // Only the top-level Desk owns this grant. Re-enter the normal device
      // launch route after a connector restart; never reuse the old grant.
      async reconnect(signal) {
        if (window.parent !== window || !location.pathname.endsWith('/desk/'))
          throw new Error('viewer recovery requires the standalone Desk entry');
        const scope = deskScope();
        await close();
        signal?.throwIfAborted();
        await refreshDeskEntry(scope, signal);
        const replacement = connectViewer(productID, scope);
        const next = await replacement;
        if (signal?.aborted) {
          await next.close();
          signal.throwIfAborted();
        }
        try { next.attach(window); }
        catch (error) { await next.close(); throw error; }
        window.__yoodeskTransportReady = replacement;
        return next;
      },
      attach(frame) {
        if (
          closed ||
          frame.location.origin !== origin ||
          !frame.location.pathname.startsWith(context.pathPrefix)
        )
          throw new Error('viewer scope changed');
        if (removers.has(frame)) return;
        const remove = installShim(link, { global: frame });
        removers.set(frame, remove);
        frame.document.documentElement.dataset.transportPath = state.path;
        frame.document.documentElement.dataset.transportState = 'connected';
        const detach = () => {
          remove();
          removers.delete(frame);
        };
        frame.addEventListener('pagehide', detach, { once: true });
      },
      close,
    };
  } catch (error) {
    if (authorized && error instanceof NegotiationInterrupted) error.deskScope = deskScope();
    removeEventListener('pagehide', leaving);
    await close().catch(() => {});
    throw error;
  }
}
