import { $, base, request, status, message, initAccess, toolbar } from './access.js';
import { connectionManager } from './connections.js';
import { textClipboard } from './clipboard.js';
import { autoReconnect } from './reconnect.js';
import { hostSidebar } from './hosts.js';
import { rdpFiles } from './rdp-files.js';
import { embedded, requestedTarget } from './viewer-bridge.js';
import scanCodes from '../../node_modules/@novnc/novnc/core/input/xtscancodes.js';

let modulePromise = null,
  iron = null,
  remote = null,
  business = null,
  generation = 0,
  phase = 'idle',
  certificate = null,
  fit = true,
  composition = false;
let inputQueue = [],
  pointer = null,
  inputFrame = 0,
  lastLocks = '';
const canvas = $('rdp-screen'),
  ime = $('rdp-ime');
const controlling = () => phase === 'active' && $('mode').value === 'control';
const audio = window.createYooDeskAudioOutput({ button: $('audio'), report: message });
let audioAvailable = false;
new MutationObserver(() => {
  $('audio').disabled = phase !== 'active' || !audioAvailable;
  if (phase !== 'active') audio.reset();
}).observe($('status'), { attributes: true, attributeFilter: ['data-state'] });
let clipboardReplies = [],
  clipboardChannelReady = false;
function clipboardAcceptance() {
  if (clipboardReplies.length >= 4) return Promise.reject(Error('剪贴板同步繁忙'));
  const item = {};
  const promise = new Promise((resolve, reject) => {
    item.resolve = resolve;
    item.reject = reject;
    item.timer = setTimeout(() => {
      clipboardReplies = clipboardReplies.filter((p) => p !== item);
      reject(Error('远端剪贴板确认超时'));
    }, 10000);
  });
  promise.catch(() => {});
  clipboardReplies.push(item);
  return promise;
}
function clipboardResponse(ok) {
  // IronRDP首先发送空格式列表完成MS-RDPECLIP初始化；该确认不属于用户复制操作。
  if (!clipboardChannelReady) {
    if (ok) {
      clipboardChannelReady = true;
      files.ready();
      clipboard.wake();
    } else message('远端未接受剪贴板通道初始化');
    return;
  }
  const item = clipboardReplies.shift();
  if (!item) return;
  clearTimeout(item.timer);
  if (ok) item.resolve();
  else item.reject(Error('远端拒绝剪贴板同步'));
}
const files = rdpFiles({
  session: () => remote,
  engine: () => iron,
  enabled: () => controlling() && clipboardChannelReady,
  report: message,
  acceptance: clipboardAcceptance,
  focused: () =>
    document.hasFocus() && !document.hidden && $('rdp-viewport').contains(document.activeElement),
  readLocal: () => navigator.clipboard.readText(),
  pasteAction: () => {
    ime.focus();
    void files.paste(pasteKeys, () => clipboard.paste(pasteKeys));
  },
  copyAction: () => {
    ime.focus();
    clipboard.copiedRemote();
    for (const code of [0x1d, 0x2e]) input(iron.DeviceEvent.keyPressed(code));
    for (const code of [0x2e, 0x1d]) input(iron.DeviceEvent.keyReleased(code));
    flush();
  },
});
let savedCredentials = null,
  acceptedCertificate = null;
let resizeTimer,
  resizeVersion = 0;
function applyResolution(attempt = 0) {
  if (!remote || !controlling()) return;
  const [w, h] = $('resolution').value.split('x').map(Number);
  if (canvas.width === w && canvas.height === h) return;
  const gen = generation,
    version = resizeVersion;
  remote.resize(w, h, 100);
  if (attempt < 15)
    resizeTimer = setTimeout(() => {
      if (gen === generation && version === resizeVersion) applyResolution(attempt + 1);
    }, 1000);
  else message('远端暂未接受分辨率调整');
}
const reconnect = autoReconnect({
  connect: () => savedCredentials && connect({ ...savedCredentials }, true),
  waiting: () => {
    $('connect').hidden = true;
    status('连接中断，正在自动重连', 'connecting');
  },
});
const clipboard = textClipboard({
  session: () => (phase === 'active' ? remote : null),
  enabled: () => controlling() && clipboardChannelReady && !files.holdsClipboard(),
  focused: () =>
    document.hasFocus() && !document.hidden && $('rdp-viewport').contains(document.activeElement),
  send: pasteClipboard,
  report: message,
});
function pasteKeys() {
  for (const code of [0x1d, 0x2f]) input(iron.DeviceEvent.keyPressed(code));
  for (const code of [0x2f, 0x1d]) input(iron.DeviceEvent.keyReleased(code));
  flush();
}
const profiles = connectionManager(async (items, saved) => {
  const old = $('target').value;
  renderTargets(items);
  $('target').value =
    saved?.target_id || (items.some((t) => t.target_id === old) ? old : items[0].target_id);
  if (!saved && old && !items.some((item) => item.target_id === old)) {
    await disconnect();
    $('target').selectedIndex = -1;
    $('connect').hidden = true;
    status('连接已删除，请选择主机');
    return;
  }
  if (saved) showCredentials();
});
function renderTargets(items) {
  $('target').replaceChildren(...items.map((t) => new Option(t.label, t.target_id)));
  // 目标目录尚未到达时禁止无效点击；RTC/TURN下加载完成晚于传输授权。
  $('connect').disabled = items.length === 0;
}
hostSidebar(profiles);
function currentTarget() {
  return profiles.items.find((t) => t.target_id === $('target').value);
}
function resizeView() {
  const stage = $('stage');
  const scale = fit
    ? Math.min(stage.clientWidth / canvas.width, stage.clientHeight / canvas.height)
    : 1;
  $('rdp-viewport').style.width = `${canvas.width * scale}px`;
  $('rdp-viewport').style.height = `${canvas.height * scale}px`;
}
new ResizeObserver(resizeView).observe($('stage'));
new MutationObserver(resizeView).observe(canvas, {
  attributes: true,
  attributeFilter: ['width', 'height'],
});
function describe(error) {
  if (typeof error?.kind === 'function') {
    const kind = error.kind();
    return (
      {
        1: '远端用户名或密码不正确',
        2: '远端登录失败，请检查域、账号与登录权限',
        3: '远端拒绝远程桌面访问',
        4: 'RDP 协商失败，请检查证书或服务端连接信息',
        5: '无法连接 RDP 业务流',
        6: 'RDP 安全协商失败',
      }[kind] || 'RDP 连接失败，请检查目标状态'
    );
  }
  return error?.message || 'RDP 操作失败';
}
function clearInputs() {
  for (const event of inputQueue) event.free();
  inputQueue = [];
}
function release() {
  clearInputs();
  pointer = null;
  cancelAnimationFrame(inputFrame);
  inputFrame = 0;
  if (remote) {
    try {
      remote.releaseAllInputs();
    } catch (error) {
      message(describe(error));
    }
  }
  updateLock();
}
let lockVersion = 0,
  lockTail = Promise.resolve();
function updateLock() {
  if (!navigator.keyboard?.lock) return;
  const version = ++lockVersion;
  lockTail = lockTail.then(async () => {
    if (version !== lockVersion) return;
    const wanted =
      document.fullscreenElement &&
      controlling() &&
      document.hasFocus() &&
      !document.hidden &&
      $('rdp-viewport').contains(document.activeElement);
    if (!wanted) {
      navigator.keyboard.unlock();
      return;
    }
    try {
      await navigator.keyboard.lock();
      if (version !== lockVersion) navigator.keyboard.unlock();
    } catch (error) {
      message(`键盘锁定未获允许：${error.message}`);
    }
  });
}
async function disconnect(automatic = false) {
  clipboardChannelReady = false;
  for (const item of clipboardReplies) {
    clearTimeout(item.timer);
    item.reject(Error('连接已断开'));
  }
  clipboardReplies = [];
  resizeVersion++;
  clearTimeout(resizeTimer);
  if (!automatic) {
    reconnect.stop();
    savedCredentials = null;
    acceptedCertificate = null;
  }
  generation++;
  phase = 'idle';
  audio.reset();
  audioAvailable = false;
  $('audio').disabled = true;
  files.reset();
  clipboard.reset();
  release();
  const old = business;
  business = null;
  certificate = null;
  $('rdp-certificate').close();
  $('rdp-password').value = '';
  if (remote) {
    try {
      remote.shutdown();
    } catch (error) {
      message(describe(error));
    }
    remote = null;
  }
  $('disconnect').disabled = true;
  $('connect').hidden = false;
  status('已断开');
  if (old)
    await request(`${base}sessions/${old.business_session_id}/`, { method: 'DELETE' }).catch(
      () => {},
    );
}
function showCredentials() {
  const target = currentTarget();
  if (!target) return;
  $('rdp-target-summary').textContent = `${target.label} · ${target.host}:${target.port}`;
  $('rdp-username').value = target.username || '';
  $('rdp-domain').value = target.domain || '';
  $('rdp-password').value = '';
  $('rdp-login-error').textContent = '';
  $('rdp-credentials').showModal();
}
function builderSet(builder, method, ...args) {
  const next = builder[method](...args);
  builder.free();
  return next;
}
const certificateKey = (payload) => JSON.stringify([payload.host, payload.port, payload.sha256]);
async function pollChallenges(session, gen) {
  let shown = null;
  while (gen === generation && phase === 'connecting') {
    try {
      const data = await request(`${base}sessions/${session.business_session_id}/challenges/`);
      if (gen !== generation || phase !== 'connecting') return;
      if (data.error) message(data.error);
      if (data.challenge && data.challenge.kind === 'certificate' && shown !== data.challenge.id) {
        shown = data.challenge.id;
        certificate = { info: data.challenge, session, gen };
        const p = data.challenge.payload;
        if (acceptedCertificate === certificateKey(p)) {
          await certificateAnswer('accept_once');
          continue;
        }
        $('rdp-certificate-details').textContent =
          `${p.host}:${p.port}\n主体：${p.subject}\n签发者：${p.issuer}\nSHA256：${p.sha256}\n有效期：${p.not_before} — ${p.not_after}`;
        $('rdp-certificate-error').textContent = '';
        $('rdp-certificate').showModal();
      }
    } catch (error) {
      if (gen === generation) message(error.message);
      return;
    }
    await new Promise((resolve) => setTimeout(resolve, 300));
  }
}
async function certificateAnswer(decision) {
  const c = certificate;
  if (!c || c.gen !== generation || c.submitting) return;
  c.submitting = true;
  if (decision === 'reject') {
    reconnect.stop();
    savedCredentials = null;
  }
  try {
    await request(
      `${base}sessions/${c.session.business_session_id}/challenges/${c.info.id}/responses/`,
      { method: 'POST', body: { kind: 'certificate', answer: { decision } } },
    );
    if (certificate !== c || c.gen !== generation) return;
    if (decision === 'accept_once') acceptedCertificate = certificateKey(c.info.payload);
    certificate = null;
    $('rdp-certificate').close();
  } catch (error) {
    if (certificate === c && c.gen === generation)
      $('rdp-certificate-error').textContent = error.message;
  } finally {
    c.submitting = false;
  }
}
$('rdp-certificate-accept').onclick = () => certificateAnswer('accept_once');
$('rdp-certificate-reject').onclick = () => certificateAnswer('reject');
$('rdp-certificate').addEventListener('cancel', () => certificateAnswer('reject'));
async function connect(credentials, automatic = false) {
  const pending = disconnect(automatic);
  const gen = generation;
  await pending;
  if (gen !== generation) return;
  const target = currentTarget();
  if (!target) return;
  savedCredentials = { ...credentials };
  reconnect.start();
  phase = 'connecting';
  status('正在协商 RDP');
  message('');
  $('disconnect').disabled = false;
  let builder = null;
  try {
    const viewer = window.__yoodeskViewer;
    const assetBase = new URL(
      `/${document.documentElement.dataset.productId}/webclient/assets/ironrdp/`,
      viewer?.origin ?? location.origin,
    );
    const moduleURL = new URL('ironrdp.js', assetBase);
    modulePromise ||= (
      viewer ? viewer.module(moduleURL) : import(/* @vite-ignore */ moduleURL.href)
    ).then(async (m) => {
      if (viewer) {
        const wasm = await fetch(new URL('ironrdp_bg.wasm', assetBase));
        if (!wasm.ok) throw new Error('RDP decoder unavailable');
        await m.default({ module_or_path: await wasm.arrayBuffer() });
      } else await m.default();
      m.setup('warn');
      return m;
    });
    iron = await modulePromise;
    if (gen !== generation) return;
    const created = await request(`${base}sessions/`, {
      method: 'POST',
      body: { protocol_version: 2, target_id: target.target_id, options: {}, client_features: [] },
    });
    if (gen !== generation) {
      await request(`${base}sessions/${created.business_session_id}/`, { method: 'DELETE' });
      return;
    }
    business = created;
    pollChallenges(created, gen);
    const url = new URL(created.stream_url, window.__yoodeskViewer?.href ?? location.href);
    url.protocol = url.protocol === 'https:' ? 'wss:' : 'ws:';
    const [w, h] = $('resolution').value.split('x').map(Number);
    canvas.width = w;
    canvas.height = h;
    resizeView();
    builder = new iron.SessionBuilder();
    for (const [method, value] of [
      ['username', credentials.username],
      ['password', credentials.password],
      ['serverDomain', credentials.domain],
      [
        'destination',
        `${target.host.includes(':') ? `[${target.host}]` : target.host}:${target.port}`,
      ],
      ['proxyAddress', url.href],
      ['authToken', 'local-access-v2'],
      ['desktopSize', new iron.DesktopSize(w, h)],
      ['renderCanvas', canvas],
      ['setCursorStyleCallbackContext', canvas],
    ])
      builder = builderSet(builder, method, value);
    builder = builderSet(builder, 'setCursorStyleCallback', (style, data, x, y) => {
      if (gen === generation)
        canvas.style.cursor =
          style === 'url'
            ? `url("${data}") ${x} ${y}, default`
            : style === 'hidden'
              ? 'none'
              : style;
    });
    builder = builderSet(builder, 'remoteClipboardChangedCallback', async (data) => {
      try {
        if (gen !== generation || !controlling()) return;
        const items = data.items();
        try {
          for (const item of items) {
            if (item.mimeType() === 'text/plain') {
              const value = item.value();
              if (typeof value === 'string') {
                files.textChanged();
                clipboard.receive(value);
              }
            }
          }
        } finally {
          for (const item of items) item.free();
        }
      } catch (error) {
        if (gen === generation) message(`剪贴板写入未获允许：${error.message}`);
      } finally {
        data.free();
      }
    });
    builder = builderSet(builder, 'extension', new iron.Extension('enable_credssp', true));
    builder = builderSet(builder, 'extension', new iron.Extension('display_control', true));
    builder = builderSet(
      builder,
      'extension',
      new iron.Extension('audio_callback', (kind, data, channels, rate) => {
        if (gen !== generation) return;
        try {
          if (kind === 'channel') {
            audioAvailable = true;
          } else if (kind === 'formats') {
            audioAvailable = data > 0;
            $('audio').disabled = phase !== 'active' || !audioAvailable;
            $('audio').dataset.available = String(data > 0);
            if (!data) message('远端没有与浏览器兼容的PCM音频格式');
          } else if (kind === 'pcm') {
            audio.pcm(data, channels, rate);
          } else if (kind === 'close') audio.reset();
          else if (kind === 'error') message(`远端音频错误：${data}`);
        } catch (error) {
          message(`声音播放失败：${error.message}`);
        }
      }),
    );
    for (const [name, callback] of [
      ['files_available_callback', files.available],
      ['file_contents_request_callback', files.requestUpload],
      ['file_contents_response_callback', files.response],
      ['lock_callback', files.lock],
      ['unlock_callback', files.unlock],
      ['locks_expired_callback', files.expired],
      ['format_list_response_callback', clipboardResponse],
    ])
      builder = builderSet(
        builder,
        'extension',
        new iron.Extension(name, (...args) => {
          if (gen === generation) return callback(...args);
        }),
      );
    builder = builderSet(
      builder,
      'extension',
      new iron.Extension('outbound_message_size_limit', 65536),
    );
    const connected = await builder.connect();
    credentials.password = '';
    builder.free();
    builder = null;
    if (gen !== generation) {
      connected.shutdown();
      return;
    }
    remote = connected;
    reconnect.ready();
    phase = 'active';
    status('已连接', 'active');
    $('connect').hidden = true;
    ime.focus();
    resizeView();
    connected
      .run()
      .then(() => {
        if (gen === generation) {
          phase = 'idle';
          release();
          status('RDP 已断开');
          $('disconnect').disabled = true;
          $('connect').hidden = false;
          remote = null;
          reconnect.retry();
        }
      })
      .catch((error) => {
        if (gen === generation) {
          phase = 'failed';
          release();
          status('RDP 连接中断', 'failed');
          message(describe(error));
          remote = null;
          $('disconnect').disabled = true;
          $('connect').hidden = false;
          reconnect.retry();
        }
      })
      .finally(() => connected.free());
  } catch (error) {
    credentials.password = '';
    if (gen === generation) {
      phase = 'failed';
      status('连接失败', 'failed');
      message(describe(error));
      $('rdp-certificate').close();
      certificate = null;
      $('disconnect').disabled = true;
      $('connect').hidden = false;
      const kind = typeof error?.kind === 'function' ? error.kind() : null;
      if ([1, 2, 3].includes(kind) || error.status === 401 || error.status === 403) {
        reconnect.stop();
        savedCredentials = null;
      } else reconnect.retry();
    }
  } finally {
    credentials.password = '';
    if (builder) builder.free();
  }
}
$('rdp-credentials-form').onsubmit = (event) => {
  event.preventDefault();
  const credentials = {
    username: $('rdp-username').value,
    password: $('rdp-password').value,
    domain: $('rdp-domain').value,
  };
  $('rdp-password').value = '';
  $('rdp-credentials').close();
  connect(credentials).catch((error) => message(describe(error)));
};
$('rdp-credentials').addEventListener('close', () => {
  $('rdp-password').value = '';
});
$('rdp-credentials-cancel').onclick = () => $('rdp-credentials').close();
$('connect').onclick = showCredentials;
$('disconnect').onclick = () => disconnect().catch((error) => message(error.message));
$('target').onchange = async () => {
  await disconnect();
  showCredentials();
};
$('connection-new').onclick = () => profiles.open();
$('connection-edit').onclick = () => profiles.open(currentTarget());
function flush() {
  inputFrame = 0;
  if (!controlling() || !remote) {
    clearInputs();
    pointer = null;
    return;
  }
  try {
    const t = new iron.InputTransaction();
    for (const e of inputQueue) t.addEvent(e);
    inputQueue = [];
    if (pointer) {
      t.addEvent(iron.DeviceEvent.mouseMove(pointer.x, pointer.y));
      pointer = null;
    }
    remote.applyInputs(t);
  } catch (error) {
    message(describe(error));
    release();
  }
}
function input(event) {
  if (!controlling()) {
    event.free();
    return;
  }
  if (inputQueue.length + (pointer ? 2 : 1) > 128) {
    event.free();
    message('输入队列繁忙，请稍后重试');
    release();
    return;
  }
  // 在点击/按键边界固定指针位置；后续鼠标移动不能把先前点击重定位到新坐标。
  if (pointer) {
    inputQueue.push(iron.DeviceEvent.mouseMove(pointer.x, pointer.y));
    pointer = null;
  }
  inputQueue.push(event);
  if (!inputFrame) inputFrame = requestAnimationFrame(flush);
}
function pointerAt(event) {
  const r = canvas.getBoundingClientRect();
  return {
    x: Math.max(
      0,
      Math.min(canvas.width - 1, Math.round(((event.clientX - r.left) * canvas.width) / r.width)),
    ),
    y: Math.max(
      0,
      Math.min(canvas.height - 1, Math.round(((event.clientY - r.top) * canvas.height) / r.height)),
    ),
  };
}
canvas.addEventListener('pointermove', (event) => {
  if (!controlling()) return;
  pointer = pointerAt(event);
  if (!inputFrame) inputFrame = requestAnimationFrame(flush);
});
canvas.addEventListener('pointerdown', (event) => {
  if (!controlling()) return;
  event.preventDefault();
  ime.focus();
  canvas.setPointerCapture(event.pointerId);
  pointer = pointerAt(event);
  input(iron.DeviceEvent.mouseButtonPressed(event.button));
});
canvas.addEventListener('pointerup', (event) => {
  if (!controlling()) return;
  pointer = pointerAt(event);
  input(iron.DeviceEvent.mouseButtonReleased(event.button));
  if (canvas.hasPointerCapture(event.pointerId)) canvas.releasePointerCapture(event.pointerId);
});
canvas.oncontextmenu = (event) => event.preventDefault();
canvas.addEventListener(
  'wheel',
  (event) => {
    if (!controlling()) return;
    event.preventDefault();
    const unit =
      [iron.RotationUnit.Pixel, iron.RotationUnit.Line, iron.RotationUnit.Page][event.deltaMode] ??
      iron.RotationUnit.Pixel;
    if (event.deltaY)
      input(
        iron.DeviceEvent.wheelRotations(
          true,
          Math.max(-32767, Math.min(32767, -Math.round(event.deltaY))),
          unit,
        ),
      );
    if (event.deltaX)
      input(
        iron.DeviceEvent.wheelRotations(
          false,
          Math.max(-32767, Math.min(32767, -Math.round(event.deltaX))),
          unit,
        ),
      );
  },
  { passive: false },
);
let handledAt = 0;
for (const element of [canvas, ime]) {
  element.addEventListener('keydown', (event) => {
    if (!controlling() || event.isComposing || composition) return;
    if ((event.ctrlKey || event.metaKey) && !event.altKey) {
      if (event.code === 'KeyV') {
        event.preventDefault();
        if (!event.repeat) {
          if (files.holdsClipboard()) void files.paste(pasteKeys, () => clipboard.paste(pasteKeys));
          else void clipboard.paste(pasteKeys);
        }
        return;
      }
      if (event.code === 'KeyC' || event.code === 'KeyX') clipboard.copiedRemote();
    }
    const code = scanCodes[event.code];
    if (!code) return;
    event.preventDefault();
    handledAt = performance.now();
    const locks = ['ScrollLock', 'NumLock', 'CapsLock', 'KanaLock'].map((key) =>
      event.getModifierState(key),
    );
    const signature = locks.join();
    if (signature !== lastLocks) {
      lastLocks = signature;
      remote.synchronizeLockKeys(...locks);
    }
    input(iron.DeviceEvent.keyPressed(code));
  });
  element.addEventListener('keyup', (event) => {
    if (!controlling() || composition) return;
    const code = scanCodes[event.code];
    if (code) {
      event.preventDefault();
      input(iron.DeviceEvent.keyReleased(code));
    }
  });
}
function textInput(text) {
  if (!controlling()) return;
  for (const char of text) {
    input(iron.DeviceEvent.unicodePressed(char));
    input(iron.DeviceEvent.unicodeReleased(char));
  }
}
ime.addEventListener('compositionstart', () => {
  composition = true;
});
ime.addEventListener('compositionend', (event) => {
  composition = false;
  textInput(event.data);
  ime.value = '';
});
ime.addEventListener('beforeinput', (event) => {
  if (
    event.inputType === 'insertText' &&
    event.data &&
    !composition &&
    performance.now() - handledAt > 50
  ) {
    event.preventDefault();
    textInput(event.data);
  }
});
ime.addEventListener('input', () => {
  if (!composition) ime.value = '';
});
ime.addEventListener('paste', async (event) => {
  if (!controlling()) return;
  event.preventDefault();
  try {
    await clipboard.paste(pasteKeys, event.clipboardData.getData('text/plain'));
  } catch (error) {
    message(error.message);
  }
});
async function pasteClipboard(text) {
  if (!remote || !controlling()) return;
  if (new TextEncoder().encode(text).length > 1048576) throw Error('剪贴板文本不能超过 1 MiB');
  const data = new iron.ClipboardData();
  data.addText('text/plain', text);
  const accepted = clipboardAcceptance();
  try {
    await remote.onClipboardPaste(data);
    await accepted;
  } finally {
    data.free();
  }
}
$('mode').onchange = () => {
  files.reset();
  clipboard.reset();
  release();
  resizeVersion++;
  clearTimeout(resizeTimer);
  applyResolution();
};
setInterval(async () => {
  await files.checkLocal();
  void clipboard.sync();
}, 750);
canvas.addEventListener('pointerdown', () => clipboard.wake());
$('fit').onclick = () => {
  fit = !fit;
  $('fit').setAttribute('aria-pressed', String(fit));
  resizeView();
};
$('resolution').onchange = () => {
  resizeVersion++;
  clearTimeout(resizeTimer);
  applyResolution();
};
$('screenshot').onclick = () => {
  if (phase !== 'active') return;
  canvas.toBlob((blob) => {
    if (!blob) return;
    const url = URL.createObjectURL(blob),
      a = document.createElement('a');
    a.href = url;
    a.download = `${document.documentElement.dataset.productName || document.documentElement.dataset.productId}-rdp-${new Date().toISOString().replace(/[:.]/g, '-')}.png`;
    a.click();
    setTimeout(() => URL.revokeObjectURL(url), 1000);
  });
};
window.addEventListener('blur', release);
window.addEventListener('focus', () => {
  clipboard.wake();
  updateLock();
});
for (const type of ['fullscreenchange', 'focusin', 'focusout', 'visibilitychange'])
  document.addEventListener(type, () => {
    if (!$('rdp-viewport').contains(document.activeElement) || document.hidden) release();
    else updateLock();
  });
window.addEventListener('pagehide', () => {
  reconnect.stop();
  savedCredentials = null;
  acceptedCertificate = null;
  generation++;
  if (remote) remote.shutdown();
});
toolbar();
initAccess(async () => {
  renderTargets(await profiles.refresh());
  if (embedded && requestedTarget) {
    if (!profiles.items.some((item) => item.target_id === requestedTarget)) {
      status('连接不存在或已删除', 'failed');
      return;
    }
    $('target').value = requestedTarget;
  }
  $('connection-new').hidden = !profiles.canManage;
  status('选择目标后点击连接');
  if (embedded && requestedTarget) showCredentials();
}, disconnect);
