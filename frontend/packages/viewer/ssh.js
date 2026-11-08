import { Terminal } from '@xterm/xterm';
import { FitAddon } from '@xterm/addon-fit';
import { SearchAddon } from '@xterm/addon-search';
import { WebLinksAddon } from '@xterm/addon-web-links';
import { $, base, request, socketURL, status, message, initAccess } from './access.js';
import { filePanel } from './sftp.js';
import { connectionManager } from './connections.js';
import { autoReconnect } from './reconnect.js';
import { sidebarLayout } from './sidebar.js';
import { hostCard } from './host-card.js';

const tabs = new Map();
const hostLayout = sidebarLayout();
let active = null,
  targets = [],
  nextTab = 0,
  challenge = null;
const profiles = connectionManager(async (items, saved) => {
  targets = items;
  renderHosts();
  for (const tab of [...tabs.values()])
    if (!items.some((item) => item.target_id === tab.target.target_id)) await remove(tab);
  if (saved) create(saved);
});
const files = filePanel(
  () => active,
  (text) => {
    $('file-error').textContent = text;
  },
);
function setStatus(tab, text, state = '') {
  tab.status = text;
  tab.state = state;
  if (active === tab) status(text, state);
}
function select(tab) {
  active = tab;
  for (const button of document.querySelectorAll('#hosts .host-connect'))
    button.setAttribute('aria-current', String(button.dataset.targetId === tab?.target.target_id));
  for (const item of tabs.values()) {
    item.node.hidden = item !== tab;
    item.button.setAttribute('aria-selected', String(item === tab));
  }
  $('empty-session').hidden = !!tab;
  if (tab) {
    status(tab.status, tab.state);
    message(tab.error || '');
    requestAnimationFrame(() => {
      tab.fit.fit();
      tab.terminal.focus();
    });
  } else status('选择主机开始连接');
  files.select();
}
async function close(tab, automatic = false) {
  if (!automatic) {
    tab.retry.stop();
    tab.credentials.clear();
    tab.trustedHosts.clear();
  }
  tab.generation++;
  tab.socket?.close();
  tab.socket = null;
  files.cancelTab(tab);
  if (challenge?.tab === tab) {
    challenge = null;
    $('ssh-challenge').close();
  }
  const session = tab.session;
  tab.session = null;
  if (session)
    await request(`${base}sessions/${session.business_session_id}/`, { method: 'DELETE' }).catch(
      () => {},
    );
}
async function remove(tab) {
  try {
    await close(tab);
  } catch (error) {
    message(error.message);
    return;
  }
  tabs.delete(tab.id);
  tab.terminal.dispose();
  tab.observer.disconnect();
  tab.node.remove();
  tab.button.remove();
  if (active === tab) select([...tabs.values()].at(-1) || null);
}
async function connect(tab) {
  const generation = tab.generation + 1;
  try {
    await close(tab, true);
    if (generation !== tab.generation) return;
    if (!tab.everConnected) tab.terminal.reset();
    tab.challengeId = null;
    tab.cachedUsed = new Set();
    tab.error = '';
    setStatus(tab, '正在连接');
    let session = await request(`${base}sessions/`, {
      method: 'POST',
      body: {
        protocol_version: 2,
        target_id: tab.target.target_id,
        options: {},
        client_features: [],
      },
    });
    if (generation !== tab.generation) {
      await request(`${base}sessions/${session.business_session_id}/`, { method: 'DELETE' });
      return;
    }
    tab.session = session;
    while (generation === tab.generation) {
      const [info, challenges] = await Promise.all([
        request(`${base}sessions/${session.business_session_id}/`),
        request(`${base}sessions/${session.business_session_id}/challenges/`),
      ]);
      if (generation !== tab.generation) return;
      tab.sftp = challenges.sftp_available;
      if (challenges.error) {
        tab.error = challenges.error;
        if (active === tab) message(challenges.error);
      }
      if (info.state === 'failed' || info.state === 'closed')
        throw new Error(challenges.error || 'SSH 连接关闭');
      if (challenges.challenge && tab.challengeId !== challenges.challenge.id) {
        const info = challenges.challenge;
        if (await reuseAuthentication(tab, info, generation)) continue;
        if (!challenge) {
          tab.challengeId = challenges.challenge.id;
          showChallenge(tab, challenges.challenge);
        }
      }
      if (info.stream_available) {
        session = info;
        tab.session = info;
        break;
      }
      await new Promise((resolve) => setTimeout(resolve, 300));
    }
    if (generation !== tab.generation) return;
    const { url, protocols } = await socketURL(session);
    if (generation !== tab.generation) return;
    const socket = new WebSocket(url, protocols);
    socket.binaryType = 'arraybuffer';
    tab.socket = socket;
    socket.onopen = () => {
      if (generation !== tab.generation) {
        socket.close();
        return;
      }
      resize(tab);
    };
    socket.onmessage = (event) => {
      if (generation !== tab.generation) return;
      if (typeof event.data !== 'string') {
        tab.terminal.write(new Uint8Array(event.data));
        return;
      }
      const v = JSON.parse(event.data);
      if (v.type === 'terminal.ready') {
        tab.retry.ready();
        tab.everConnected = true;
        setStatus(tab, '已连接', 'active');
        tab.fit.fit();
        resize(tab);
        if (active === tab) {
          files.select();
          tab.terminal.focus();
        }
      }
      if (v.type === 'terminal.error') {
        tab.error = v.message;
        setStatus(tab, v.message, 'failed');
      }
      if (v.type === 'terminal.exit') {
        tab.terminal.writeln(`\r\n[终端已退出：${v.exit_code ?? v.signal ?? ''}]`);
        setStatus(tab, '终端已退出');
      }
    };
    socket.onclose = () => {
      if (generation !== tab.generation) return;
      setStatus(tab, '连接中断，正在自动重连', 'connecting');
      tab.sftp = false;
      files.cancelTab(tab);
      if (active === tab) files.select();
      tab.retry.retry();
    };
    socket.onerror = () => {
      if (generation === tab.generation) setStatus(tab, 'SSH 数据连接失败', 'failed');
    };
  } catch (error) {
    if (generation !== tab.generation) return;
    tab.error = error.message;
    setStatus(tab, '连接失败', 'failed');
    if (active === tab) message(error.message);
    if (error.status === 401 || error.status === 403) tab.retry.stop();
    else {
      if (/credentials rejected|authentication/i.test(error.message)) tab.credentials.clear();
      tab.retry.retry();
    }
  }
}
function resize(tab) {
  if (tab.socket?.readyState === WebSocket.OPEN)
    tab.socket.send(
      JSON.stringify({
        v: 1,
        type: 'terminal.resize',
        cols: Math.min(1000, Math.max(1, tab.terminal.cols)),
        rows: Math.min(1000, Math.max(1, tab.terminal.rows)),
      }),
    );
}
function create(target) {
  const existing = [...tabs.values()].find((tab) => tab.target.target_id === target.target_id);
  if (existing) {
    select(existing);
    return;
  }
  if (tabs.size >= 8) {
    message('最多同时打开 8 个标签');
    return;
  }
  const node = document.createElement('div');
  node.className = 'terminal-pane';
  $('terminals').append(node);
  // xterm 的主题/尺寸样式使用页面独有 nonce，不放宽全站 CSP，也不修改全局 document。
  const documentOverride = new Proxy(document, {
    get(target, name) {
      if (name === 'createElement')
        return (tag, options) => {
          const element = target.createElement(tag, options);
          if (tag.toLowerCase() === 'style')
            element.nonce = document.documentElement.dataset.styleNonce;
          return element;
        };
      const value = Reflect.get(target, name, target);
      return typeof value === 'function' ? value.bind(target) : value;
    },
  });
  const terminal = new Terminal({
    documentOverride,
    cursorBlink: true,
    fontFamily: 'Consolas, "Noto Sans Mono", monospace',
    fontSize: 14,
    scrollback: 5000,
    theme: {
      background: '#ffffff',
      foreground: '#333542',
      cursor: '#2563eb',
      selectionBackground: '#dbeafe',
      black: '#333542',
      red: '#b42318',
      green: '#287b45',
      yellow: '#996600',
      blue: '#2563eb',
      magenta: '#8b3db4',
      cyan: '#007c91',
      white: '#737c8c',
      brightBlack: '#737c8c',
      brightRed: '#b42318',
      brightGreen: '#287b45',
      brightYellow: '#996600',
      brightBlue: '#2563eb',
      brightMagenta: '#8b3db4',
      brightCyan: '#007c91',
      brightWhite: '#333542',
    },
  });
  const fit = new FitAddon(),
    search = new SearchAddon();
  terminal.loadAddon(fit);
  terminal.loadAddon(search);
  terminal.loadAddon(new WebLinksAddon());
  terminal.open(node);
  const button = document.createElement('div');
  button.className = 'ssh-tab';
  button.setAttribute('role', 'tab');
  button.tabIndex = 0;
  const name = document.createElement('span');
  name.textContent = target.label;
  const closeButton = document.createElement('button');
  closeButton.textContent = '×';
  closeButton.setAttribute('aria-label', `关闭 ${target.label}`);
  button.append(name, closeButton);
  $('tabs').append(button);
  const tab = {
    id: ++nextTab,
    target,
    node,
    button,
    terminal,
    fit,
    search,
    generation: 0,
    socket: null,
    session: null,
    sftp: false,
    path: '',
    entries: [],
    status: '未连接',
    state: '',
    challengeId: null,
    credentials: new Map(),
    trustedHosts: new Map(),
    cachedUsed: new Set(),
    everConnected: false,
  };
  tab.retry = autoReconnect({
    connect: () => connect(tab),
    waiting: () => setStatus(tab, '连接中断，正在自动重连', 'connecting'),
  });
  tab.retry.start();
  button.onclick = () => select(tab);
  button.onkeydown = (event) => {
    if (event.key === 'Enter') select(tab);
  };
  closeButton.onclick = (event) => {
    event.stopPropagation();
    remove(tab);
  };
  tab.observer = new ResizeObserver(() => {
    if (active === tab) {
      fit.fit();
      resize(tab);
    }
  });
  tab.observer.observe(node);
  terminal.onData((data) => {
    if (tab.socket?.readyState === WebSocket.OPEN && tab.state === 'active') {
      const bytes = new TextEncoder().encode(data);
      if (tab.socket.bufferedAmount + bytes.byteLength > 1 << 20) {
        message('终端发送拥塞，请等待后重试');
        return;
      }
      tab.socket.send(bytes);
    }
  });
  terminal.onResize(() => resize(tab));
  tabs.set(tab.id, tab);
  select(tab);
  connect(tab);
  if (innerWidth < 600) hostLayout.collapse();
}
function showChallenge(tab, info) {
  select(tab);
  challenge = { tab, info, generation: tab.generation };
  $('challenge-title').textContent =
    info.kind === 'host_key'
      ? '核对 SSH 主机指纹'
      : info.kind === 'credentials'
        ? 'SSH 身份验证'
        : '交互认证';
  $('challenge-host').textContent =
    `${info.payload.username || tab.target.username}@${info.payload.host}:${info.payload.port} · 第 ${info.payload.hop_index + 1} 个连接`;
  $('challenge-description').textContent =
    info.kind === 'host_key'
      ? `${info.payload.algorithm}\n${info.payload.fingerprint}\n${info.payload.changed ? `指纹已改变！原指纹 ${info.payload.previous_fingerprint}` : '请与管理员核对，仅接受本次连接'}`
      : info.payload.instruction || '';
  $('credential-fields').hidden = info.kind !== 'credentials';
  $('interactive-fields').replaceChildren();
  $('challenge-error').textContent = '';
  for (const prompt of info.payload.prompts || []) {
    const label = document.createElement('label');
    label.textContent = prompt.prompt;
    const input = document.createElement('input');
    input.type = prompt.echo ? 'text' : 'password';
    input.dataset.promptId = prompt.id;
    input.autocomplete = 'off';
    input.maxLength = 4096;
    label.append(input);
    $('interactive-fields').append(label);
  }
  $('challenge-submit').disabled = false;
  $('challenge-submit').textContent = info.kind === 'host_key' ? '接受本次连接' : '继续';
  $('ssh-challenge').showModal();
}
const authenticationKey = (info) =>
  JSON.stringify([
    info.payload.host,
    info.payload.port,
    info.payload.username,
    info.payload.hop_index,
  ]);
async function reuseAuthentication(tab, info, generation) {
  const key = authenticationKey(info);
  let answer;
  if (
    info.kind === 'host_key' &&
    !info.payload.changed &&
    tab.trustedHosts.get(key) === info.payload.fingerprint
  )
    answer = { decision: 'accept_once' };
  else if (info.kind === 'credentials' && tab.credentials.has(key) && !tab.cachedUsed.has(key)) {
    tab.cachedUsed.add(key);
    const body = tab.credentials.get(key);
    if (body.kind === 'keyboard_interactive') answer = { credential_ref: 'keyboard_interactive' };
    else {
      const c = await request(`${base}credentials/`, { method: 'POST', body });
      answer = { credential_ref: c.credential_ref };
    }
  }
  if (!answer) return false;
  if (generation !== tab.generation) return true;
  tab.challengeId = info.id;
  await request(
    `${base}sessions/${tab.session.business_session_id}/challenges/${info.id}/responses/`,
    { method: 'POST', body: { kind: info.kind, answer } },
  );
  return true;
}
function clearSecrets() {
  for (const id of ['ssh-password', 'private-key', 'key-passphrase', 'private-key-file'])
    $(id).value = '';
  $('interactive-fields').replaceChildren();
}
$('auth-method').onchange = () => {
  $('password-field').hidden = $('auth-method').value !== 'password';
  $('key-fields').hidden = $('auth-method').value !== 'private_key';
};
$('private-key-file').onchange = async () => {
  const current = challenge;
  const file = $('private-key-file').files[0];
  if (!current || !file) return;
  if (file.size > 262144) {
    $('challenge-error').textContent = '私钥文件不能超过 256 KiB';
    return;
  }
  const contents = await file.text();
  if (
    challenge === current &&
    current.generation === current.tab.generation &&
    $('private-key-file').files[0] === file
  )
    $('private-key').value = contents;
};
$('challenge-form').onsubmit = async (event) => {
  event.preventDefault();
  const current = challenge;
  if (!current) return;
  const { tab, info, generation } = current;
  $('challenge-submit').disabled = true;
  try {
    let answer;
    let cached;
    if (info.kind === 'host_key') answer = { decision: 'accept_once' };
    else if (info.kind === 'credentials') {
      const kind = $('auth-method').value;
      if (kind === 'keyboard_interactive') {
        answer = { credential_ref: 'keyboard_interactive' };
        cached = { kind };
      } else {
        const body = {
          kind,
          secret: kind === 'password' ? $('ssh-password').value : $('private-key').value,
          persist: false,
        };
        if (kind === 'private_key') body.passphrase = $('key-passphrase').value;
        const c = await request(`${base}credentials/`, { method: 'POST', body });
        cached = body;
        answer = { credential_ref: c.credential_ref };
      }
    } else
      answer = {
        answers: [...$('interactive-fields').querySelectorAll('input')].map((i) => ({
          id: i.dataset.promptId,
          value: i.value,
        })),
      };
    if (challenge !== current || generation !== tab.generation) return;
    await request(
      `${base}sessions/${tab.session.business_session_id}/challenges/${info.id}/responses/`,
      { method: 'POST', body: { kind: info.kind, answer } },
    );
    if (challenge !== current || generation !== tab.generation) return;
    if (cached) tab.credentials.set(authenticationKey(info), cached);
    if (info.kind === 'host_key')
      tab.trustedHosts.set(authenticationKey(info), info.payload.fingerprint);
    clearSecrets();
    challenge = null;
    $('ssh-challenge').close();
  } catch (error) {
    if (challenge === current) $('challenge-error').textContent = error.message;
  } finally {
    if (challenge === current) $('challenge-submit').disabled = false;
  }
};
const cancelChallenge = () => {
  const tab = challenge?.tab;
  challenge = null;
  clearSecrets();
  $('ssh-challenge').close();
  if (tab)
    close(tab)
      .then(() => setStatus(tab, '已取消'))
      .catch((e) => message(e.message));
};
$('challenge-cancel').onclick = cancelChallenge;
$('ssh-challenge').addEventListener('cancel', cancelChallenge);
$('new-tab').onclick = () => {
  profiles.open();
};
$('connection-new').onclick = () => profiles.open();
$('connection-new-tab').onclick = () => profiles.open();
$('files-toggle').onclick = () => {
  $('sftp-dock').hidden = !$('sftp-dock').hidden;
  files.select();
};
$('files-close').onclick = () => {
  $('sftp-dock').hidden = true;
};
$('fullscreen').onclick = async () => {
  if (document.fullscreenElement) await document.exitFullscreen();
  else await document.documentElement.requestFullscreen();
};
$('search-next').onclick = () => active?.search.findNext($('terminal-search').value);
$('search-prev').onclick = () => active?.search.findPrevious($('terminal-search').value);
$('terminal-search').onkeydown = (event) => {
  if (event.key === 'Enter') active?.search.findNext(event.target.value);
};
$('copy-terminal').onclick = async () => {
  if (active) {
    try {
      await navigator.clipboard.writeText(active.terminal.getSelection());
    } catch (error) {
      message(error.message);
    }
  }
};
$('paste-terminal').onclick = async () => {
  const tab = active;
  if (!tab || tab.state !== 'active') return;
  try {
    const value = await navigator.clipboard.readText();
    if (tab !== active) return;
    if (value.includes('\n') && !confirm('粘贴包含多行，可能立即执行命令。确认粘贴？')) return;
    tab.terminal.paste(value);
  } catch (error) {
    message(error.message);
  }
};
function renderHosts() {
  $('hosts').replaceChildren(
    ...targets
      .filter((t) =>
        `${t.label} ${t.host}`.toLowerCase().includes($('host-filter').value.toLowerCase()),
      )
      .map((target) => {
        return hostCard(target, {
          protocol: 'ssh',
          detail: `${target.username}@${target.host}:${target.port}${target.jump_chain.length ? ` · ${target.jump_chain.length} 跳` : ''}`,
          current: active?.target.target_id === target.target_id,
          connect: () => create(target),
          edit: profiles.canManage ? () => profiles.open(target) : null,
          remove: profiles.canManage
            ? () => void profiles.remove(target).catch((error) => message(error.message))
            : null,
        });
      }),
  );
}
$('host-filter').oninput = renderHosts;
window.addEventListener('pagehide', () => {
  for (const tab of tabs.values()) {
    tab.retry.stop();
    tab.credentials.clear();
    tab.trustedHosts.clear();
    tab.generation++;
    tab.socket?.close();
  }
});
initAccess(
  async () => {
    targets = await profiles.refresh();
    $('connection-new').hidden = !profiles.canManage;
    $('connection-new-tab').hidden = !profiles.canManage;
    $('new-tab').disabled = !profiles.canManage;
    renderHosts();
    if (new URLSearchParams(window.__yoodeskViewer?.query ?? location.search).get('gateway') === '1' && targets.length === 1)
      await create(targets[0]);
  },
  async () => {
    for (const tab of [...tabs.values()]) await remove(tab);
  },
);
