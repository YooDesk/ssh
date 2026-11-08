let isolatedRenewal = Promise.resolve();
// 邀请凭据只从片段读取到内存并立即清除，不进入 HTTP URL、历史记录或持久存储。
const invitationFragment = new URLSearchParams(location.hash.slice(1));
let invitationCredential = invitationFragment.get('invite') || '';
if (invitationFragment.has('invite')) history.replaceState(history.state, '', location.pathname + location.search);
// 所有 viewer 共用本机访问会话；凭据只在当前登录请求内存在。
export const $ = (id) => document.getElementById(id);
export const product = document.documentElement.dataset.productId;
export const capability = document.documentElement.dataset.capability;
export const base = `/${product}/webclient/${capability}/api/v2/`;
const connectorAccess = new URLSearchParams(window.__yoodeskViewer?.query ?? location.search).get('gateway') === '1';
// 必须先安装传输，再执行访问密码/邀请认证；LAN 完全不加载网关资源。
const transportReady = connectorAccess
  ? import(/* @vite-ignore */ `/${product}/transport/viewer.js`).then(module => module.attachViewer(product))
  : Promise.resolve();
transportReady.catch(() => {});
const accessBase = connectorAccess
  ? `/${product}/webclient/${capability}/connector-access/v2/`
  : `/${product}/webclient/access/v2/`;
let csrf = '';
let authenticated = false;
let accessInstance = '',
  accessEpoch = 0;
export async function request(url, options = {}) {
  if (
    authenticated &&
    options.method &&
    options.method !== 'GET' &&
    url !== `${accessBase}login/`
  ) {
    const expectedInstance = accessInstance,
      epoch = accessEpoch;
    options = {
      ...options,
      headers: {
        ...options.headers,
        'X-Request-ID': options.headers?.['X-Request-ID'] || crypto.randomUUID(),
        'X-Access-Instance': expectedInstance,
      },
    };
    const write = async () => {
      const refresh = async () => {
        // 前置读取也必须响应取消，且不能无限挂起。
        const signal = options.signal
          ? AbortSignal.any([options.signal, AbortSignal.timeout(15000)])
          : AbortSignal.timeout(15000);
        const info = await sendRequest(`${accessBase}session/`, { signal });
        if (
          !authenticated ||
          epoch !== accessEpoch ||
          !expectedInstance ||
          info.accessInstance !== expectedInstance
        ) {
          throw new DOMException('访问会话已变更，旧操作已取消', 'AbortError');
        }
        csrf = info.csrf_token;
      };
      await refresh();
      let result;
      try {
        result = await sendRequest(url, options);
      } catch (error) {
        // 仅明确在鉴权阶段拒绝、尚未执行的请求允许重发一次；未知结果绝不重放。
        if (!error.csrfRejected || options.signal?.aborted) throw error;
        await refresh();
        result = await sendRequest(url, options);
      }
      if (url === `${accessBase}session/renew/`) csrf = result.csrf_token;
      return result;
    };
    if (url !== `${accessBase}session/renew/`) return write();
    // 只协调续期；远端慢I/O不会阻塞其他标签的写入和关闭。
    options.signal = options.signal
      ? AbortSignal.any([options.signal, AbortSignal.timeout(15000)])
      : AbortSignal.timeout(15000);
    if (self.origin === 'null') {
      const result = isolatedRenewal.then(() => {
        options.signal.throwIfAborted();
        return write();
      });
      isolatedRenewal = result.catch(() => {});
      return result;
    }
    return navigator.locks
      ? navigator.locks.request(`${product}-access-renew`, { signal: options.signal }, write)
      : write();
  }
  return sendRequest(url, options);
}
async function sendRequest(url, options = {}) {
  // 取消单个等待请求不关闭其他标签共用的连接，但必须立即结束该请求。
  if (options.signal) {
    const signal = options.signal;
    signal.throwIfAborted();
    await new Promise((resolve, reject) => {
      const abort = () => reject(signal.reason);
      signal.addEventListener('abort', abort, {once: true});
      transportReady.then(resolve, reject).finally(() => signal.removeEventListener('abort', abort));
    });
  } else await transportReady;
  options.signal?.throwIfAborted();
  const headers = { ...options.headers };
  if (options.method && options.method !== 'GET') {
    headers['X-CSRF-Token'] = csrf;
    headers['X-Request-ID'] ||= crypto.randomUUID();
    if (
      typeof options.body === 'object' &&
      !(options.body instanceof Blob) &&
      !(options.body instanceof ArrayBuffer)
    ) {
      headers['Content-Type'] = 'application/json';
      options = { ...options, body: JSON.stringify(options.body) };
    }
  }
  const response = await fetch(url, { ...options, headers, credentials: 'same-origin' });
  if (response.status === 204) return null;
  const value = await response.json();
  if (!response.ok) {
    if (response.status === 401 && csrf) document.dispatchEvent(new Event('access-expired'));
    const error = new Error(value.error?.message || `HTTP ${response.status}`);
    error.status = response.status;
    error.csrfRejected =
      response.status === 400 &&
      value.error?.code === 'invalid_request' &&
      value.error?.retryable === true &&
      value.error?.details?.stage === 'csrf' &&
      value.error?.details?.field === 'x-csrf-token';
    throw error;
  }
  if (value.data && url.startsWith(accessBase)) {
    Object.defineProperty(value.data, 'accessInstance', {
      value: response.headers?.get('x-access-instance') || '',
    });
  }
  return value.data;
}
export async function socketURL(session, channel = 'stream') {
  const ticket = await request(`${base}sessions/${session.business_session_id}/stream-tickets/`, {
    method: 'POST',
    body: { channel },
  });
  const url = new URL(channel === 'events' ? session.event_url : session.stream_url, (window.__yoodeskViewer?.href ?? location.href));
  url.protocol = url.protocol === 'https:' ? 'wss:' : 'ws:';
  return {
    url: url.href,
    protocols: [
      { vnc: 'binary', rdp: 'rdcleanpath.v1', ssh: 'terminal.v1' }[capability],
      `raccess.ticket.${ticket.ticket}`,
    ],
  };
}
export function status(text, state = '') {
  $('status').textContent = text;
  $('status').dataset.state = state;
  if ($('connection-state')) $('connection-state').textContent = text;
}
export function message(text) {
  $('message').textContent = text;
}
export async function initAccess(connected, disconnected) {
  let established = false;
  let renewalTimer,
    generation = 0;
  function stopRenewal() {
    generation++;
    clearTimeout(renewalTimer);
  }
  function scheduleRenewal(info) {
    clearTimeout(renewalTimer);
    const current = generation;
    // 页面打开期间允许持续观看/传输；绝对期限仍由服务端强制执行。
    const remaining = Date.parse(info.expires_at) - Date.now();
    const delay = Math.max(1000, Math.min(300000, remaining / 2));
    renewalTimer = setTimeout(async () => {
      if (!established || generation !== current) return;
      try {
        const renewed = await request(`${accessBase}session/renew/`, { method: 'POST' });
        if (established && generation === current) scheduleRenewal(renewed);
      } catch (error) {
        if (!established || generation !== current) return;
        message(`访问续期失败：${error.message}`);
        // 网络恢复后重试续期，不重放任何业务写操作。
        renewalTimer = setTimeout(() => {
          if (established && generation === current) scheduleRenewal(info);
        }, 10000);
      }
    }, delay);
  }
  document.addEventListener('access-expired', () => {
    if (!established) return;
    established = false;
    authenticated = false;
    accessInstance = '';
    accessEpoch++;
    stopRenewal();
    csrf = '';
    void disconnected().catch(() => {});
    document.body.classList.add('login-required');
    $('login').hidden = false;
    $('login-error').textContent = '访问验证已过期，请重新登录';
  });
  async function establish() {
    const info = await request(`${accessBase}session/`);
    if (!info.accessInstance) throw new Error('服务器缺少访问会话实例标识，请更新服务端');
    accessInstance = info.accessInstance;
    accessEpoch++;
    csrf = info.csrf_token;
    document.body.classList.remove('login-required');
    $('login').hidden = true;
    established = true;
    authenticated = true;
    stopRenewal();
    scheduleRenewal(info);
    await connected();
  }
  $('login-form').addEventListener('submit', async (event) => {
    event.preventDefault();
    accessEpoch++;
    const button = event.submitter;
    button.disabled = true;
    $('login-error').textContent = '';
    try {
      if (!connectorAccess) {
        const pre = await request(`${accessBase}bootstrap/`);
        csrf = pre.preauth_csrf;
      }
      await request(`${accessBase}login/`, {
        method: 'POST',
        body: { access_mode: 'cookie', credential: $('credential').value },
      });
      $('credential').value = '';
      await establish();
    } catch (error) {
      $('login-error').textContent = error.message;
    } finally {
      button.disabled = false;
    }
  });
  $('login-form').dataset.ready = '1';
  $('logout').onclick = async () => {
    stopRenewal();
    try {
      await disconnected();
      await request(`${accessBase}logout/`, { method: 'POST' });
      authenticated = false;
      accessInstance = '';
      accessEpoch++;
      if (window.__yoodeskViewer) window.__yoodeskViewer.reload();
      else location.reload();
    } catch (error) {
      message(error.message);
      if (established) scheduleRenewal({ expires_at: new Date(Date.now() + 20000).toISOString() });
    }
  };
  window.addEventListener('pagehide', stopRenewal);
  window.addEventListener('pageshow', async (event) => {
    if (!event.persisted || !established) return;
    try {
      scheduleRenewal(await request(`${accessBase}session/`));
    } catch (error) {
      message(error.message);
    }
  });
  try {
    if (invitationCredential) {
      const credential = invitationCredential;
      invitationCredential = '';
      if (!connectorAccess) {
        const pre = await request(`${accessBase}bootstrap/`);
        csrf = pre.preauth_csrf;
      }
      await request(`${accessBase}login/`, { method: 'POST', body: { access_mode: 'cookie', credential } });
    }
    await establish();
  } catch (error) {
    $('login').hidden = false;
    $('login-error').textContent = error.message.includes('required') ? '' : error.message;
  }
}

// 与 desk 一致：固定只影响自动隐藏，始终允许拖动。
export function toolbar() {
  const bar = $('toolbar');
  let pinned = false,
    timer;
  const reveal = () => {
    bar.classList.remove('auto-hidden');
    clearTimeout(timer);
    if (!pinned)
      timer = setTimeout(() => {
        if (!bar.matches(':hover') && !bar.contains(document.activeElement))
          bar.classList.add('auto-hidden');
      }, 2200);
  };
  $('toolbar-pin').onclick = () => {
    pinned = !pinned;
    $('toolbar-pin').setAttribute('aria-pressed', String(pinned));
    reveal();
  };
  bar.addEventListener('pointerenter', reveal);
  bar.addEventListener('pointerleave', reveal);
  $('toolbar-reveal').addEventListener('pointerenter', reveal);
  const handle = $('toolbar-drag');
  let drag;
  handle.onpointerdown = (event) => {
    const r = bar.getBoundingClientRect();
    drag = { x: event.clientX - r.left, y: event.clientY - r.top };
    handle.setPointerCapture(event.pointerId);
    reveal();
  };
  handle.onpointermove = (event) => {
    if (!drag) return;
    bar.style.transform = 'none';
    bar.style.left = `${Math.max(0, Math.min(innerWidth - bar.offsetWidth, event.clientX - drag.x))}px`;
    bar.style.top = `${Math.max(0, Math.min(innerHeight - bar.offsetHeight, event.clientY - drag.y))}px`;
  };
  handle.onpointerup = () => {
    drag = null;
    reveal();
  };
  $('fullscreen').onclick = async () => {
    try {
      if (document.fullscreenElement) await document.exitFullscreen();
      else await document.documentElement.requestFullscreen();
    } catch (error) {
      message(error.message);
    }
  };
  reveal();
}
