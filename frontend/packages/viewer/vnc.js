import RFB from '@novnc/novnc';
import { $, base, request, socketURL, status, message, initAccess, toolbar } from './access.js';
import { connectionManager } from './connections.js';
import { textClipboard } from './clipboard.js';
import { autoReconnect } from './reconnect.js';
import { hostSidebar } from './hosts.js';
import { embedded, requestedTarget } from './viewer-bridge.js';

let rfb = null,
  session = null,
  generation = 0,
  connected = false;
let clientEnded = true;
let credentials;
const reconnect = autoReconnect({
  connect,
  waiting: () => status('连接中断，正在自动重连', 'connecting'),
});
const profiles = connectionManager(async (items, saved) => {
  const previous = $('target').value;
  $('target').replaceChildren(...items.map((t) => new Option(t.label, t.target_id)));
  $('target').value =
    saved?.target_id ||
    (items.some((t) => t.target_id === previous) ? previous : items[0].target_id);
  if (!saved && previous && !items.some((item) => item.target_id === previous)) {
    await disconnect();
    $('target').selectedIndex = -1;
    status('连接已删除，请选择主机');
    return;
  }
  if (saved || $('target').value !== previous) {
    credentials = undefined;
    reconnect.stop();
    await connect();
  }
});
const canControl = () => connected && $('mode').value === 'control';
hostSidebar(profiles);
const clipboard = textClipboard({
  session: () => (connected ? rfb : null),
  enabled: canControl,
  focused: () =>
    document.hasFocus() && !document.hidden && $('stage').contains(document.activeElement),
  send: (text) => rfb.clipboardPasteFrom(text),
  report: message,
});
const pasteKeys = () => {
  rfb.sendKey(0xffe3, 'ControlLeft', true);
  rfb.sendKey(0x76, 'KeyV', true);
  rfb.sendKey(0x76, 'KeyV', false);
  rfb.sendKey(0xffe3, 'ControlLeft', false);
};
let lockVersion = 0,
  lockQueue = Promise.resolve();
const wantKeyboardLock = () =>
  !!document.fullscreenElement &&
  canControl() &&
  document.hasFocus() &&
  !document.hidden &&
  $('stage').contains(document.activeElement);
function updateKeyboardLock() {
  if (!navigator.keyboard?.lock) return;
  const version = ++lockVersion;
  lockQueue = lockQueue
    .catch(() => {})
    .then(async () => {
      if (version !== lockVersion) return;
      if (!wantKeyboardLock()) {
        navigator.keyboard.unlock();
        return;
      }
      try {
        await navigator.keyboard.lock();
        if (version !== lockVersion || !wantKeyboardLock()) navigator.keyboard.unlock();
      } catch (error) {
        if (version === lockVersion) message(`系统快捷键锁定失败：${error.message}`);
      }
    });
}
function apply() {
  if (!rfb) return;
  rfb.viewOnly = $('mode').value !== 'control' || !document.hasFocus();
  rfb.scaleViewport = $('scaling').value === 'fit';
  rfb.resizeSession = $('scaling').value === 'resize';
  rfb.clipViewport = $('scaling').value === 'clip';
  rfb.dragViewport = $('drag-viewport').checked && $('vnc-display').value === 'all';
  rfb.showDotCursor = $('dot-cursor').checked;
  rfb.qualityLevel = Number($('quality').value);
  rfb.compressionLevel = Number($('compression').value);
  for (const id of ['ctrl-alt-del', 'send-key', 'power-action']) $(id).disabled = !canControl();
  updateKeyboardLock();
}
async function disconnect(keepGeneration = false) {
  if (!keepGeneration) {
    generation++;
    reconnect.stop();
    credentials = undefined;
  }
  connected = false;
  clipboard.reset();
  const old = session;
  session = null;
  if (rfb && !clientEnded) rfb.disconnect();
  rfb = null;
  updateKeyboardLock();
  $('stage').replaceChildren();
  $('vnc-auth').close();
  $('server-verify').close();
  if (old)
    await request(`${base}sessions/${old.business_session_id}/`, { method: 'DELETE' }).catch(
      () => {},
    );
}
async function connect() {
  const gen = ++generation;
  const targetId = $('target').value;
  reconnect.start();
  try {
    await disconnect(true);
    if (gen !== generation) return;
    status('正在连接');
    message('');
    const created = await request(`${base}sessions/`, {
      method: 'POST',
      body: { protocol_version: 2, target_id: targetId, options: {}, client_features: [] },
    });
    if (gen !== generation) {
      await request(`${base}sessions/${created.business_session_id}/`, { method: 'DELETE' });
      return;
    }
    session = created;
    const target = profiles.items.find((item) => item.target_id === targetId);
    const { url, protocols } = await socketURL(created);
    if (gen !== generation) return;
    const client = new RFB($('stage'), url, {
      shared: true,
      wsProtocols: protocols,
      credentials: credentials || (target?.username ? { username: target.username } : undefined),
    });
    rfb = client;
    clientEnded = false;
    $('vnc-display').replaceChildren(new Option('全部显示器', 'all'));
    $('vnc-display').hidden = true;
    apply();
    client.addEventListener('screens', (event) => {
      if (gen !== generation) return;
      const previous = $('vnc-display').value;
      const screens = event.detail.screens;
      $('vnc-display').replaceChildren(
        new Option('全部显示器', 'all'),
        ...screens.map(
          (screen, index) =>
            new Option(`显示器${index + 1} (${screen.width}×${screen.height})`, screen.id),
        ),
      );
      $('vnc-display').hidden = screens.length <= 1;
      const selected = screens.some((screen) => screen.id === previous) ? previous : 'all';
      $('vnc-display').value = selected;
      client.selectScreen(selected === 'all' ? null : selected);
      $('scaling').querySelector('[value=resize]').disabled = screens.length > 1;
      if (screens.length > 1 && $('scaling').value === 'resize') $('scaling').value = 'fit';
      apply();
    });
    client.addEventListener('connect', () => {
      if (gen !== generation) return;
      connected = true;
      reconnect.ready();
      status('已连接', 'active');
      $('connection-state').textContent = '已连接';
      apply();
      client.focus();
    });
    client.addEventListener('disconnect', (event) => {
      if (gen !== generation) return;
      clientEnded = true;
      connected = false;
      status(event.detail.clean ? '已断开' : '连接中断', 'failed');
      reconnect.retry();
      $('vnc-auth').close();
      $('server-verify').close();
      apply();
    });
    client.addEventListener('securityfailure', (event) => {
      if (gen === generation) {
        reconnect.stop();
        credentials = undefined;
        message(`VNC 认证失败：${event.detail.reason || event.detail.status}`);
      }
    });
    client.addEventListener('credentialsrequired', (event) => {
      if (gen !== generation) return;
      for (const name of ['username', 'password', 'target'])
        $(`vnc-${name}-label`).hidden = !event.detail.types.includes(name);
      $('vnc-username').value = credentials?.username || target?.username || '';
      $('vnc-password').value = '';
      $('vnc-target').value = '';
      $('vnc-auth').showModal();
      if (event.detail.types.includes('username') && !$('vnc-username').value)
        $('vnc-username').focus();
      else $('vnc-password').focus();
    });
    client.addEventListener('serververification', (event) => {
      if (gen !== generation) return;
      const bytes = new Uint8Array(event.detail.publickey);
      $('server-fingerprint').textContent =
        `${event.detail.type}: ${Array.from(bytes, (b) => b.toString(16).padStart(2, '0')).join(':')}`;
      $('server-verify').showModal();
    });
    client.addEventListener('desktopname', (event) => {
      if (gen !== generation) return;
      $('desktop-name').textContent = event.detail.name;
    });
    client.addEventListener('capabilities', () => {
      if (gen === generation) $('power').hidden = !client.capabilities.power;
    });
    client.addEventListener('bell', () => {
      if (gen !== generation || !$('bell').checked) return;
      message('远端响铃');
    });
    client.addEventListener('clipboard', (event) => {
      if (gen !== generation || !canControl()) return;
      clipboard.receive(event.detail.text);
    });
  } catch (error) {
    if (gen === generation) {
      status('连接失败', 'failed');
      message(error.message);
      if (error.status === 401 || error.status === 403) reconnect.stop();
      else reconnect.retry();
    }
  }
}
toolbar();
$('mode').onchange = () => {
  clipboard.reset();
  apply();
};
$('vnc-display').onchange = () => {
  if (!rfb) return;
  rfb.selectScreen($('vnc-display').value === 'all' ? null : $('vnc-display').value);
  apply();
};
for (const id of ['scaling', 'drag-viewport', 'dot-cursor', 'quality', 'compression'])
  $(id).onchange = apply;
$('fit').onclick = () => {
  $('scaling').value = $('scaling').value === 'fit' ? 'native' : 'fit';
  $('fit').setAttribute('aria-pressed', String($('scaling').value === 'fit'));
  apply();
};
$('settings').onclick = () => {
  $('settings-panel').hidden = !$('settings-panel').hidden;
};
$('settings-close').onclick = () => {
  $('settings-panel').hidden = true;
};
$('target').onchange = () => {
  credentials = undefined;
  reconnect.stop();
  void connect();
};
$('connection-new').onclick = () => profiles.open();
$('connection-edit').onclick = () =>
  profiles.open(profiles.items.find((t) => t.target_id === $('target').value));
$('ctrl-alt-del').onclick = () => {
  if (canControl()) rfb.sendCtrlAltDel();
};
$('send-key').onclick = () => {
  if (!canControl()) return;
  const keys = {
    Escape: [0xff1b, 'Escape'],
    Tab: [0xff09, 'Tab'],
    Super: [0xffeb, 'MetaLeft'],
    CapsLock: [0xffe5, 'CapsLock'],
    NumLock: [0xff7f, 'NumLock'],
    F1: [0xffbe, 'F1'],
    F11: [0xffc8, 'F11'],
    F12: [0xffc9, 'F12'],
  };
  const [code, key] = keys[$('special-key').value];
  rfb.sendKey(code, key);
  rfb.focus();
};
$('screenshot').onclick = () => {
  if (!connected) return;
  rfb.toBlob((blob) => {
    if (!blob) return;
    const url = URL.createObjectURL(blob);
    const a = document.createElement('a');
    a.href = url;
    a.download = `${document.documentElement.dataset.productName || document.documentElement.dataset.productId}-vnc-${new Date().toISOString().replace(/[:.]/g, '-')}.png`;
    a.click();
    setTimeout(() => URL.revokeObjectURL(url), 1000);
  });
};
$('power-action').onclick = () => {
  if (!canControl() || !rfb.capabilities.power) return;
  const action = $('power-select').value;
  if (confirm(`确认执行远端${$('power-select').selectedOptions[0].textContent}？`)) rfb[action]();
};
$('vnc-auth-form').onsubmit = (event) => {
  event.preventDefault();
  if (rfb)
    rfb.sendCredentials(
      (credentials = {
        username: $('vnc-username').value,
        password: $('vnc-password').value,
        target: $('vnc-target').value,
      }),
    );
  $('vnc-password').value = '';
  $('vnc-auth').close();
};
$('vnc-auth-cancel').onclick = () => {
  disconnect().catch((e) => message(e.message));
};
$('vnc-auth').addEventListener('cancel', () => disconnect().catch((e) => message(e.message)));
$('vnc-auth').addEventListener('close', () => {
  $('vnc-password').value = '';
});
$('server-accept').onclick = () => {
  rfb?.approveServer();
  $('server-verify').close();
};
$('server-reject').onclick = () => disconnect().catch((e) => message(e.message));
$('server-verify').addEventListener('cancel', () => disconnect().catch((e) => message(e.message)));
window.addEventListener('blur', apply);
window.addEventListener('focus', () => {
  clipboard.wake();
  apply();
});
for (const event of ['fullscreenchange', 'focusin', 'focusout', 'visibilitychange'])
  document.addEventListener(event, updateKeyboardLock);
setInterval(() => clipboard.sync(), 750);
$('stage').addEventListener('pointerdown', () => clipboard.wake(), true);
$('stage').addEventListener(
  'keydown',
  (event) => {
    if (!canControl() || !(event.ctrlKey || event.metaKey)) return;
    if (event.code === 'KeyV' && !event.altKey) {
      event.preventDefault();
      event.stopImmediatePropagation();
      if (!event.repeat) void clipboard.paste(pasteKeys);
    } else if (event.code === 'KeyC' || event.code === 'KeyX') clipboard.copiedRemote();
  },
  true,
);
$('stage').addEventListener(
  'paste',
  (event) => {
    if (!canControl()) return;
    event.preventDefault();
    event.stopImmediatePropagation();
    void clipboard.paste(pasteKeys, event.clipboardData.getData('text/plain'));
  },
  true,
);
window.addEventListener('pagehide', () => {
  reconnect.stop();
  credentials = undefined;
  generation++;
  rfb?.disconnect();
});
initAccess(async () => {
  const items = await profiles.refresh();
  $('connection-new').hidden = !profiles.canManage;
  $('target').replaceChildren(...items.map((t) => new Option(t.label, t.target_id)));
  if (embedded && requestedTarget) {
    if (!items.some((item) => item.target_id === requestedTarget)) {
      status('连接不存在或已删除', 'failed');
      return;
    }
    $('target').value = requestedTarget;
  }
  if (!items.length) {
    status('没有配置 VNC 目标');
    return;
  }
  await connect();
}, disconnect);
