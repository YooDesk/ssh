import { $, capability, product } from './access.js';

// 三种viewer共用侧栏尺寸和折叠交互；仅持久化无敏感信息的显示偏好。
export function sidebarLayout() {
  const sidebar = $('hosts-sidebar'),
    toggle = $('hosts-toggle'),
    key = `${product}.${capability}.sidebar`;
  const initial = capability === 'ssh' ? 210 : 230;
  let width = initial,
    collapsed = innerWidth < (capability === 'ssh' ? 600 : 700),
    drag = null;
  try {
    const saved = JSON.parse(localStorage.getItem(key));
    if (Number.isFinite(saved?.width)) width = saved.width;
    if (typeof saved?.collapsed === 'boolean') collapsed = saved.collapsed;
  } catch {
    /* 浏览器禁止偏好存储时，当前页面仍可调整布局。 */
  }
  const title = document.createElement('div'),
    close = document.createElement('button'),
    expand = document.createElement('button'),
    grip = document.createElement('div');
  title.className = 'sidebar-title';
  close.type = 'button';
  close.id = 'hosts-collapse';
  close.textContent = '‹';
  close.title = '收起主机列表';
  close.setAttribute('aria-label', close.title);
  const heading = sidebar.querySelector('h1');
  heading.replaceWith(title);
  title.append(heading, close);
  expand.id = 'hosts-expand';
  expand.type = 'button';
  expand.textContent = '›';
  expand.title = '展开主机列表';
  expand.setAttribute('aria-label', expand.title);
  grip.id = 'hosts-resize';
  grip.tabIndex = 0;
  grip.setAttribute('role', 'separator');
  grip.setAttribute('aria-orientation', 'vertical');
  grip.setAttribute('aria-label', '调整主机列表宽度');
  grip.setAttribute('aria-controls', 'hosts-sidebar');
  document.body.append(expand, grip);
  function maxWidth() {
    return Math.max(
      180,
      Math.min(
        520,
        innerWidth -
          Math.max(0, sidebar.getBoundingClientRect().left) -
          (innerWidth <= 700 ? 48 : 320),
      ),
    );
  }
  const clamp = (value) => Math.max(180, Math.min(maxWidth(), value));
  function save() {
    try {
      localStorage.setItem(key, JSON.stringify({ width, collapsed }));
    } catch {
      /* 可选的本机显示偏好。 */
    }
  }
  function update() {
    document.body.classList.toggle('hosts-hidden', collapsed);
    const value = clamp(width);
    document.body.style.setProperty('--hosts-width', `${value}px`);
    sidebar.setAttribute('aria-hidden', String(collapsed));
    toggle.setAttribute('aria-expanded', String(!collapsed));
    toggle.setAttribute('aria-controls', 'hosts-sidebar');
    toggle.title = collapsed ? '展开主机列表' : '收起主机列表';
    expand.hidden = !collapsed;
    grip.hidden = collapsed;
    grip.setAttribute('aria-valuemin', '180');
    grip.setAttribute('aria-valuemax', String(maxWidth()));
    grip.setAttribute('aria-valuenow', String(value));
    grip.style.left = `${sidebar.getBoundingClientRect().right - 4}px`;
  }
  function collapse(value = true) {
    collapsed = value;
    update();
    save();
    if (collapsed) expand.focus();
  }
  toggle.onclick = () => collapse(!collapsed);
  close.onclick = () => collapse();
  expand.onclick = () => collapse(false);
  grip.onpointerdown = (event) => {
    if (event.button !== 0) return;
    event.preventDefault();
    drag = { x: event.clientX, width: clamp(width) };
    grip.setPointerCapture(event.pointerId);
    document.body.classList.add('sidebar-resizing');
  };
  grip.onpointermove = (event) => {
    if (!drag) return;
    width = clamp(drag.width + event.clientX - drag.x);
    update();
  };
  const stop = () => {
    if (!drag) return;
    drag = null;
    document.body.classList.remove('sidebar-resizing');
    save();
  };
  grip.onpointerup = (event) => {
    stop();
    if (grip.hasPointerCapture(event.pointerId)) grip.releasePointerCapture(event.pointerId);
  };
  grip.onpointercancel = stop;
  grip.onlostpointercapture = stop;
  window.addEventListener('blur', stop);
  grip.onkeydown = (event) => {
    if (!['ArrowLeft', 'ArrowRight', 'Home', 'End'].includes(event.key)) return;
    event.preventDefault();
    width =
      event.key === 'Home'
        ? 180
        : event.key === 'End'
          ? maxWidth()
          : clamp(width) + (event.key === 'ArrowLeft' ? -16 : 16);
    width = clamp(width);
    update();
    save();
  };
  grip.ondblclick = () => {
    width = initial;
    update();
    save();
  };
  window.addEventListener('resize', update);
  new ResizeObserver(update).observe(sidebar);
  update();
  return { collapse };
}
