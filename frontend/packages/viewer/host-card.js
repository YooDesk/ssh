// 主机信息与操作分行，窄侧栏中操作按钮也不参与主机名称的宽度分配。
export function hostCard(target, { protocol, detail, current = false, connect, edit, remove }) {
  const card = document.createElement('li');
  card.className = 'host-card';
  const button = document.createElement('button'),
    mark = document.createElement('span'),
    text = document.createElement('span'),
    name = document.createElement('span'),
    address = document.createElement('small');
  button.type = 'button';
  button.className = 'host-connect';
  button.dataset.targetId = target.target_id;
  button.setAttribute('aria-current', String(current));
  button.title = `${target.label}\n${detail}`;
  button.onclick = connect;
  mark.className = 'host-mark';
  mark.textContent = protocol === 'ssh' ? '>_' : '▣';
  mark.setAttribute('aria-hidden', 'true');
  text.className = 'host-text';
  name.className = 'host-name';
  name.textContent = target.label;
  address.className = 'host-address';
  address.textContent = detail;
  text.append(name, address);
  button.append(mark, text);
  card.append(button);
  if (edit || remove) {
    const actions = document.createElement('div');
    actions.className = 'host-actions';
    if (target.is_default) {
      const tag = document.createElement('span');
      tag.className = 'host-tag';
      tag.textContent = '本机';
      actions.append(tag);
    }
    function action(label, kind, path, handler) {
      const item = document.createElement('button');
      item.type = 'button';
      item.className = `host-action host-${kind}${kind === 'edit' ? ' connection-edit' : ''}`;
      item.title = `${label}连接`;
      item.setAttribute('aria-label', `${label} ${target.label}`);
      const svg = document.createElementNS('http://www.w3.org/2000/svg', 'svg'),
        line = document.createElementNS('http://www.w3.org/2000/svg', 'path');
      svg.setAttribute('viewBox', '0 0 24 24');
      svg.setAttribute('aria-hidden', 'true');
      line.setAttribute('d', path);
      svg.append(line);
      const caption = document.createElement('span');
      caption.textContent = label;
      item.append(svg, caption);
      item.onclick = handler;
      actions.append(item);
    }
    if (edit) action('编辑', 'edit', 'M14 5l5 5M4 20l4-1 12-12a2.8 2.8 0 0 0-4-4L4 15z', edit);
    if (remove && !target.is_default)
      action('删除', 'delete', 'M3 6h18M9 6V3h6v3M5 6l1 15h12l1-15M10 10v7M14 10v7', remove);
    card.append(actions);
  }
  return card;
}
