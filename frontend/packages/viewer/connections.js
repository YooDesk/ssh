// 已授权管理用户先保存目标，再用id创建业务会话。配置不保存口令或私钥。
import { $, base, capability, request } from './access.js';
export function connectionManager(changed) {
  let items = [],
    canManage = false,
    editing = null,
    jumps = [];
  async function refresh() {
    const data = await request(`${base}connection-targets/`);
    items = data.items;
    canManage = data.can_manage;
    return items;
  }
  function renderJumps() {
    $('connection-jumps').replaceChildren(
      ...jumps.map((id, index) => {
        const li = document.createElement('li'),
          name = document.createElement('span');
        name.textContent = items.find((t) => t.target_id === id)?.label || '连接已删除';
        li.append(name);
        for (const [label, move] of [
          ['上移', -1],
          ['下移', 1],
          ['移除', 0],
        ]) {
          const b = document.createElement('button');
          b.type = 'button';
          b.textContent = label;
          b.onclick = () => {
            if (move) {
              const next = index + move;
              if (next < 0 || next >= jumps.length) return;
              [jumps[index], jumps[next]] = [jumps[next], jumps[index]];
            } else jumps.splice(index, 1);
            renderJumps();
          };
          li.append(b);
        }
        return li;
      }),
    );
  }
  function open(target = null) {
    if (!canManage) return;
    editing = target;
    jumps = [...(target?.jump_chain || [])];
    $('connection-error').textContent = '';
    $('connection-title').textContent = target ? '编辑连接' : '新建连接';
    $('connection-label').value = target?.label || '';
    $('connection-host').value = target?.host || '';
    $('connection-host').readOnly = !!target?.is_default;
    $('connection-port').value = String(
      target?.port || { ssh: 22, rdp: 3389, vnc: 5900 }[capability],
    );
    $('connection-rdp-fields').hidden = capability !== 'rdp';
    $('connection-vnc-fields').hidden = capability !== 'vnc';
    $('connection-vnc-username').value = target?.username || '';
    $('connection-rdp-username').value = target?.username || '';
    $('connection-domain').value = target?.domain || '';
    $('connection-certificate').value = target?.certificate_sha256 || '';
    $('connection-username').value =
      target?.username || items.find((t) => t.is_default)?.username || '';
    $('connection-fingerprint').value = target?.host_key || '';
    $('connection-root').value = target?.root || '/';
    $('connection-ssh-fields').hidden = capability !== 'ssh';
    $('connection-username').required = capability === 'ssh';
    $('connection-jump').replaceChildren(
      new Option('选择已保存的 SSH 连接', ''),
      ...items
        .filter((t) => t.target_id !== target?.target_id && !t.jump_chain.length)
        .map((t) => new Option(`${t.label} (${t.host})`, t.target_id)),
    );
    $('connection-delete').hidden = !target || target.is_default;
    renderJumps();
    $('connection-dialog').showModal();
    $('connection-label').focus();
  }
  $('connection-add-jump').onclick = () => {
    const id = $('connection-jump').value;
    if (!id) return;
    if (jumps.length === 8) {
      $('connection-error').textContent = '最多 8 个跳板';
      return;
    }
    if (jumps.includes(id)) {
      $('connection-error').textContent = '不能重复经过同一个跳板';
      return;
    }
    jumps.push(id);
    renderJumps();
  };
  $('connection-cancel').onclick = () => $('connection-dialog').close();
  $('connection-form').onsubmit = async (event) => {
    event.preventDefault();
    $('connection-save').disabled = true;
    $('connection-error').textContent = '';
    try {
      const body = {
        label: $('connection-label').value,
        host: $('connection-host').value,
        port: Number($('connection-port').value),
        ...(capability === 'ssh'
          ? {
              username: $('connection-username').value,
              host_key: $('connection-fingerprint').value,
              root: $('connection-root').value,
              jump_chain: jumps,
            }
          : capability === 'rdp'
            ? {
                username: $('connection-rdp-username').value,
                domain: $('connection-domain').value,
                certificate_sha256: $('connection-certificate').value,
              }
            : { username: $('connection-vnc-username').value }),
      };
      if (editing) body.expected_revision = editing.revision;
      const saved = await request(
        `${base}connection-targets/${editing ? `${editing.target_id}/` : ''}`,
        { method: editing ? 'PATCH' : 'POST', body },
      );
      await refresh();
      $('connection-dialog').close();
      await changed(items, saved);
    } catch (error) {
      $('connection-error').textContent = error.message;
    } finally {
      $('connection-save').disabled = false;
    }
  };
  async function remove(target) {
    if (!canManage || !target || target.is_default || !confirm(`删除连接“${target.label}”？`))
      return;
    await request(`${base}connection-targets/${target.target_id}/`, {
      method: 'DELETE',
      body: { expected_revision: target.revision },
    });
    await refresh();
    $('connection-dialog').close();
    await changed(items, null);
  }
  $('connection-delete').onclick = async () => {
    try {
      await remove(editing);
    } catch (error) {
      $('connection-error').textContent = error.message;
    }
  };
  return {
    refresh,
    open,
    remove,
    get items() {
      return items;
    },
    get canManage() {
      return canManage;
    },
  };
}
