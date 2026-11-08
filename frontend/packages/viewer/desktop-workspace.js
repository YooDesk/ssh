import { $, capability, product, status, message, initAccess } from './access.js';
import { connectionManager } from './connections.js';
import { sidebarLayout } from './sidebar.js';
import { hostCard } from './host-card.js';

// 每个iframe对应一个独立原生viewer上下文，标签切换保留其WS、认证和重连状态。
const tabs = new Map(),
  layout = sidebarLayout();
let active = null,
  nextId = 0;
const profiles = connectionManager(async (items, saved) => {
  await closeTabs(
    [...tabs.values()].filter(
      (tab) => !items.some((item) => item.target_id === tab.target.target_id),
    ),
  );
  renderHosts();
  if (saved) create(saved);
});
function renderHosts() {
  $('target').replaceChildren(
    ...profiles.items.map((target) => new Option(target.label, target.target_id)),
  );
  if (active) $('target').value = active.target.target_id;
  else $('target').selectedIndex = -1;
  const filter = $('host-filter').value.toLowerCase();
  $('hosts').replaceChildren(
    ...profiles.items
      .filter((target) => `${target.label} ${target.host}`.toLowerCase().includes(filter))
      .map((target) =>
        hostCard(target, {
          protocol: capability,
          detail: `${target.host}:${target.port}`,
          current: active?.target.target_id === target.target_id,
          connect: () => create(target),
          edit: profiles.canManage ? () => profiles.open(target) : null,
          remove: profiles.canManage
            ? () => void profiles.remove(target).catch((error) => message(error.message))
            : null,
        }),
      ),
  );
}
function select(tab) {
  active = tab;
  for (const item of tabs.values()) {
    item.frame.hidden = item !== tab;
    item.node.setAttribute('aria-selected', String(item === tab));
    item.node.tabIndex = item === tab ? 0 : -1;
  }
  $('empty-session').hidden = !!tab;
  $('files-toggle').disabled = !tab?.ready || tab?.state !== 'active';
  $('screenshot').disabled = !tab?.ready || tab?.state !== 'active';
  status(tab?.status || '从左侧选择主机开始连接', tab?.state || '');
  message(tab?.message || '');
  $('workspace-target').textContent = tab ? `${tab.target.host}:${tab.target.port}` : '';
  if (tab) {
    $('target').value = tab.target.target_id;
    tab.frame.contentWindow?.focus();
  }
  renderHosts();
}
async function closeTabs(items) {
  for (const tab of items) {
    if (tab.closing) continue;
    tab.closing = true;
    const saves = [];
    tab.frame.contentDocument?.dispatchEvent(
      new CustomEvent('yoodesk-stop-recording', { detail: saves }),
    );
    await Promise.all(saves);
    const selectedClosing = active === tab;
    tabs.delete(tab.id);
    tab.frame.remove();
    tab.node.remove();
    if (selectedClosing) select([...tabs.values()].at(-1) || null);
  }
}
function close(tab) {
  closeTabs([tab]);
}
function create(target) {
  const existing = [...tabs.values()].find((tab) => tab.target.target_id === target.target_id);
  if (existing) {
    select(existing);
    return;
  }
  if (tabs.size >= 8) {
    message('最多同时打开 8 个会话，请先关闭不需要的标签');
    return;
  }
  const tab = {
    id: ++nextId,
    target: { ...target },
    ready: false,
    status: '正在连接',
    state: 'connecting',
    message: '',
  };
  tab.node = document.createElement('div');
  tab.node.className = 'desktop-tab';
  tab.node.id = `desktop-tab-${tab.id}`;
  tab.node.setAttribute('role', 'tab');
  tab.node.setAttribute('aria-label', target.label);
  tab.node.setAttribute('aria-controls', `desktop-frame-${tab.id}`);
  const dot = document.createElement('span'),
    name = document.createElement('span'),
    button = document.createElement('button');
  dot.className = 'tab-status';
  name.className = 'tab-name';
  name.textContent = target.label;
  button.type = 'button';
  button.className = 'tab-close';
  button.textContent = '×';
  button.setAttribute('aria-label', `关闭 ${target.label}`);
  tab.node.title = `${target.label}\n${target.host}:${target.port}`;
  tab.node.append(dot, name, button);
  tab.node.onclick = () => select(tab);
  button.onclick = (event) => {
    event.stopPropagation();
    close(tab);
  };
  tab.node.onauxclick = (event) => {
    if (event.button === 1) {
      event.preventDefault();
      close(tab);
    }
  };
  tab.node.onkeydown = (event) => {
    if (event.target !== tab.node) return;
    if (event.key === 'Enter' || event.key === ' ') {
      event.preventDefault();
      select(tab);
    }
    if (event.key === 'Delete') {
      event.preventDefault();
      if (!event.repeat) {
        close(tab);
        active?.node.focus();
      }
    }
  };
  tab.frame = document.createElement('iframe');
  tab.frame.className = 'desktop-frame';
  tab.frame.id = `desktop-frame-${tab.id}`;
  tab.frame.dataset.tabId = String(tab.id);
  tab.frame.title = `${capability.toUpperCase()} · ${target.label}`;
  tab.frame.allow = 'clipboard-read; clipboard-write; fullscreen';
  const url = new URL(`/${product}/webclient/${capability}/`, location.origin);
  url.searchParams.set('viewer', '1');
  if (new URLSearchParams(location.search).get('gateway') === '1') url.searchParams.set('gateway', '1');
  url.searchParams.set('target', target.target_id);
  tab.frame.src = url.href;
  tab.frame.onload = () => {
    if (!tabs.has(tab.id)) return;
    tab.ready = true;
    if (active === tab) $('files-toggle').disabled = tab.state !== 'active';
  };
  tabs.set(tab.id, tab);
  $('tabs').append(tab.node);
  $('desktop-content').append(tab.frame);
  select(tab);
  if (innerWidth < 700) layout.collapse();
}
window.addEventListener('message', (event) => {
  if (event.origin !== location.origin) return;
  const tab = [...tabs.values()].find((item) => item.frame.contentWindow === event.source);
  if (!tab) return;
  if (event.data?.type === 'yoodesk.viewer-access-expired') {
    document.dispatchEvent(new Event('access-expired'));
    return;
  }
  if (event.data?.type !== 'yoodesk.viewer-state') return;
  tab.status = String(event.data.status || '').slice(0, 160);
  tab.state = ['active', 'failed', 'connecting'].includes(event.data.state) ? event.data.state : '';
  tab.message = String(event.data.message || '').slice(0, 500);
  tab.node.dataset.state = tab.state;
  if (active === tab) {
    status(tab.status, tab.state);
    message(tab.message);
    $('files-toggle').disabled = tab.state !== 'active';
    $('screenshot').disabled = tab.state !== 'active';
  }
});
$('host-filter').oninput = renderHosts;
$('connection-new').onclick = () => profiles.open();
$('connection-new-tab').onclick = () => profiles.open();
$('new-tab').onclick = () => {
  layout.collapse(false);
  $('host-filter').focus();
};
$('files-toggle').hidden = capability !== 'rdp';
$('files-toggle').onclick = () =>
  active?.frame.contentDocument?.getElementById('files-toggle')?.click();
$('screenshot').onclick = () => {
  const button = active?.frame.contentDocument?.getElementById('screenshot');
  if (!button || button.disabled) { message('尚未收到桌面画面，暂时无法截图'); return; }
  button.click();
};
$('fullscreen').onclick = async () => {
  if (document.fullscreenElement) await document.exitFullscreen();
  else await document.documentElement.requestFullscreen();
};
function closeAll() {
  return closeTabs([...tabs.values()]);
}
window.addEventListener('pagehide', closeAll);
window.addEventListener('beforeunload', (event) => {
  if (
    [...tabs.values()].some((tab) => {
      const button = tab.frame.contentDocument?.querySelector('[data-recording]');
      return button && button.dataset.recording !== 'idle';
    })
  ) {
    event.preventDefault();
    event.returnValue = '';
  }
});
initAccess(
  async () => {
    const items = await profiles.refresh();
    $('hosts-title').textContent = `${capability.toUpperCase()} 主机`;
    $('connection-new').hidden = !profiles.canManage;
    $('connection-new-tab').hidden = !profiles.canManage;
    $('target').replaceChildren(
      ...items.map((target) => new Option(target.label, target.target_id)),
    );
    renderHosts();
    if (capability === 'vnc' && !tabs.size && items.length) create(items[0]);
  },
  async () => closeAll(),
);
