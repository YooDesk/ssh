import { $, base, request } from './access.js';

// 文件操作绑定发起时的 SSH 标签与业务会话，切标签不会把响应写进其他主机。
export function filePanel(current, error) {
  let listing = 0,
    queued = Promise.resolve(),
    action = null;
  const tasks = new Set();
  const sessionURL = (tab) => `${base}sessions/${tab.session.business_session_id}/`;
  const join = (path, name) => (path ? `${path}/${name}` : name);
  function dialog(title, description, initial = null) {
    return new Promise((resolve) => {
      action = resolve;
      $('file-action-title').textContent = title;
      $('file-action-description').textContent = description;
      $('file-action-label').hidden = initial === null;
      $('file-action-name').value = initial || '';
      $('file-action').showModal();
      if (initial !== null) $('file-action-name').focus();
    });
  }
  const done = (value) => {
    $('file-action').close();
    action?.(value);
    action = null;
  };
  $('file-action-form').onsubmit = (event) => {
    event.preventDefault();
    const value = $('file-action-label').hidden ? true : $('file-action-name').value.trim();
    if (
      typeof value === 'string' &&
      (!value || value.includes('/') || value.includes('\\') || value === '.' || value === '..')
    )
      return;
    done(value);
  };
  $('file-action-cancel').onclick = () => done(null);
  $('file-action').addEventListener('cancel', () => done(null));
  async function list() {
    const tab = current(),
      gen = ++listing;
    error('');
    $('remote-files').replaceChildren();
    if (!tab?.session || !tab.sftp || tab.state !== 'active') {
      error(tab ? '当前 SSH 连接尚未提供 SFTP' : '先连接 SSH 主机');
      return;
    }
    const sid = tab.session.business_session_id,
      path = tab.path;
    try {
      const entries = [];
      let cursor = null;
      do {
        const data = await request(
          `${sessionURL(tab)}entries/?path=${encodeURIComponent(path)}${cursor ? `&cursor=${encodeURIComponent(cursor)}` : ''}`,
        );
        entries.push(...data.items);
        cursor = data.next_cursor;
        if (gen !== listing || sid !== tab.session?.business_session_id) return;
      } while (cursor);
      tab.entries = entries;
      if (gen !== listing || current() !== tab) return;
      render(tab);
    } catch (e) {
      if (gen === listing && current() === tab) error(e.message);
    }
  }
  function render(tab) {
    $('file-host').textContent = `${tab.target.username}@${tab.target.host}`;
    $('breadcrumbs').replaceChildren();
    const parts = tab.path.split('/').filter(Boolean);
    for (let i = 0; i <= parts.length; i++) {
      const b = document.createElement('button');
      b.textContent = i === 0 ? '根目录' : parts[i - 1];
      b.onclick = () => {
        tab.path = parts.slice(0, i).join('/');
        list();
      };
      if (i === parts.length) b.setAttribute('aria-current', 'location');
      $('breadcrumbs').append(b);
    }
    $('remote-files').replaceChildren(
      ...tab.entries
        .filter((e) => $('show-hidden').checked || !e.name.startsWith('.'))
        .map((entry) => {
          const li = document.createElement('li'),
            name = document.createElement('button'),
            size = document.createElement('small');
          name.className = 'entry-name';
          name.textContent = `${entry.type === 'directory' ? '▸ ' : entry.type === 'symlink' ? '↗ ' : ''}${entry.name}`;
          size.textContent = entry.type === 'directory' ? '文件夹' : `${entry.size} B`;
          li.append(name, size);
          name.onclick = () => {
            if (entry.type === 'directory') {
              tab.path = entry.path;
              list();
            } else download(tab, entry);
          };
          for (const [label, fn] of [
            [
              '改名',
              async () => {
                const to = await dialog('重命名', entry.name, entry.name);
                if (to === null) return;
                await request(`${sessionURL(tab)}entries/`, {
                  method: 'PATCH',
                  body: { from: entry.path, to: join(tab.path, to) },
                });
              },
            ],
            [
              '删除',
              async () => {
                if (
                  !(await dialog(
                    '删除',
                    `确认删除 ${entry.name}${entry.type === 'directory' ? ' 及其全部内容' : ''}？`,
                  ))
                )
                  return;
                await request(`${sessionURL(tab)}entries/`, {
                  method: 'DELETE',
                  body: { path: entry.path, recursive: entry.type === 'directory' },
                });
              },
            ],
          ]) {
            const button = document.createElement('button');
            button.className = 'entry-action';
            button.textContent = label;
            button.onclick = async () => {
              try {
                await fn();
                if (current() === tab) await list();
              } catch (e) {
                error(e.message);
              }
            };
            li.append(button);
          }
          return li;
        }),
    );
  }
  function makeTask(tab, name) {
    const item = document.createElement('li'),
      title = document.createElement('div'),
      progress = document.createElement('progress'),
      detail = document.createElement('small'),
      cancel = document.createElement('button');
    title.textContent = `${name} · ${tab.target.label}`;
    progress.max = 100;
    progress.value = 0;
    cancel.textContent = '取消';
    item.append(title, progress, detail, cancel);
    $('transfers').append(item);
    const task = {
      tab,
      sid: tab.session.business_session_id,
      url: sessionURL(tab),
      name,
      item,
      progress,
      detail,
      cancel,
      controller: new AbortController(),
      cancelled: false,
      finished: false,
      id: null,
      offset: 0,
      total: 0,
      start: performance.now(),
    };
    task.cancel.onclick = () => {
      task.cancelled = true;
      task.controller.abort();
    };
    tasks.add(task);
    return task;
  }
  function check(task) {
    if (task.cancelled || task.sid !== task.tab.session?.business_session_id)
      throw new DOMException('传输已取消', 'AbortError');
  }
  function update(task) {
    const percent = task.total ? Math.floor((task.offset / task.total) * 100) : 100;
    task.progress.value = percent;
    task.detail.textContent = `${percent}% · ${task.offset}/${task.total} B · ${Math.round(task.offset / Math.max(0.1, (performance.now() - task.start) / 1000) / 1024)} KiB/s `;
  }
  async function cleanup(task) {
    if (!task.id) return;
    try {
      await request(`${task.url}transfers/${task.id}/`, { method: 'DELETE' });
    } catch (e) {
      if (task.tab.session?.business_session_id === task.sid) error(`传输清理失败：${e.message}`);
    }
  }
  async function run(task, body) {
    queued = queued.then(async () => {
      task.start = performance.now();
      try {
        check(task);
        await body();
        if (!task.committed) check(task);
        task.progress.value = 100;
        task.detail.textContent = '已完成';
      } catch (e) {
        task.detail.textContent =
          task.cancelled || e.name === 'AbortError' ? '已取消' : `失败：${e.message}`;
        await cleanup(task);
      } finally {
        task.finished = true;
        task.cancel.remove();
      }
    });
    return queued;
  }
  function upload(tab, file, path) {
    const task = makeTask(tab, file.name);
    task.total = file.size;
    run(task, async () => {
      // 创建响应即使在取消后到达也记下编号并在 finally 回收。
      const created = await request(`${task.url}transfers/`, {
        method: 'POST',
        body: {
          direction: 'upload',
          path: join(path, file.name),
          conflict: 'fail',
          size: String(file.size),
        },
      });
      task.id = created.transfer_id;
      check(task);
      while (task.offset < file.size) {
        const bytes = await file.slice(task.offset, task.offset + 1048576).arrayBuffer();
        check(task);
        await request(`${task.url}transfers/${task.id}/content/`, {
          method: 'PATCH',
          headers: {
            'Content-Type': 'application/octet-stream',
            'Upload-Offset': String(task.offset),
          },
          body: bytes,
          signal: task.controller.signal,
        });
        check(task);
        task.offset += bytes.byteLength;
        update(task);
      }
      await request(`${task.url}transfers/${task.id}/complete/`, {
        method: 'POST',
        body: { size: String(file.size) },
      });
      task.committed = true;
      if (current() === tab && path === tab.path) await list();
    });
  }
  async function download(tab, entry) {
    if (!tab.session) return;
    let sink = null;
    try {
      if (Number(entry.size) > 128 * 1024 * 1024 && window.showSaveFilePicker) {
        const handle = await window.showSaveFilePicker({ suggestedName: entry.name });
        sink = await handle.createWritable();
      } else if (Number(entry.size) > 128 * 1024 * 1024) {
        throw new Error('此浏览器不支持流式保存；128 MiB 以上文件请使用支持文件保存 API 的浏览器');
      }
    } catch (e) {
      if (e.name !== 'AbortError') error(e.message);
      return;
    }
    const task = makeTask(tab, entry.name);
    task.total = Number(entry.size);
    const chunks = [];
    run(task, async () => {
      try {
        const created = await request(`${task.url}transfers/`, {
          method: 'POST',
          body: {
            direction: 'download',
            path: entry.path,
            conflict: 'fail',
            source_version: entry.version,
          },
        });
        task.id = created.transfer_id;
        check(task);
        while (task.offset < task.total) {
          const end = Math.min(task.total, task.offset + 1048576) - 1;
          const response = await fetch(`${task.url}transfers/${task.id}/content/`, {
            headers: { Range: `bytes=${task.offset}-${end}` },
            signal: task.controller.signal,
          });
          if (!response.ok) {
            const v = await response.json();
            throw new Error(v.error?.message || `HTTP ${response.status}`);
          }
          const bytes = await response.arrayBuffer();
          check(task);
          if (sink) await sink.write(bytes);
          else chunks.push(bytes);
          task.offset += bytes.byteLength;
          update(task);
        }
        check(task);
        await request(`${task.url}transfers/${task.id}/complete/`, {
          method: 'POST',
          body: { size: String(task.total) },
        });
        if (sink) await sink.close();
        else {
          const url = URL.createObjectURL(new Blob(chunks));
          const a = document.createElement('a');
          a.href = url;
          a.download = entry.name;
          a.click();
          setTimeout(() => URL.revokeObjectURL(url), 1000);
        }
      } catch (e) {
        if (sink) await sink.abort();
        throw e;
      }
    });
  }
  $('file-refresh').onclick = list;
  $('show-hidden').onchange = () => {
    if (current()) render(current());
  };
  $('file-up').onclick = () => {
    const tab = current();
    if (!tab) return;
    tab.path = tab.path.split('/').slice(0, -1).join('/');
    list();
  };
  $('file-mkdir').onclick = async () => {
    const tab = current();
    if (!tab?.sftp) return;
    const name = await dialog('新建文件夹', '创建在当前远程目录', '');
    if (name === null) return;
    try {
      await request(`${sessionURL(tab)}entries/`, {
        method: 'POST',
        body: { path: join(tab.path, name) },
      });
      await list();
    } catch (e) {
      error(e.message);
    }
  };
  $('file-new').onclick = async () => {
    const tab = current();
    if (!tab?.sftp) return;
    const name = await dialog('新建文件', '创建空文件', '');
    if (name !== null) upload(tab, new File([], name), tab.path);
  };
  const uploadFiles = (items) => {
    const tab = current();
    if (!tab?.sftp || tab.state !== 'active') {
      error('请先连接支持 SFTP 的 SSH 主机');
      return;
    }
    if ([...tasks].filter((t) => !t.finished).length + items.length > 64) {
      error('最多排队 64 个文件');
      return;
    }
    for (const file of items) upload(tab, file, tab.path);
  };
  $('file-upload').onchange = () => {
    uploadFiles([...$('file-upload').files]);
    $('file-upload').value = '';
  };
  $('file-drop').ondragover = (event) => {
    event.preventDefault();
    $('file-drop').classList.add('dragging');
  };
  $('file-drop').ondragleave = () => $('file-drop').classList.remove('dragging');
  $('file-drop').ondrop = (event) => {
    event.preventDefault();
    $('file-drop').classList.remove('dragging');
    uploadFiles([...event.dataTransfer.files]);
  };
  $('clear-transfers').onclick = () => {
    for (const task of tasks)
      if (task.finished) {
        task.item.remove();
        tasks.delete(task);
      }
  };
  return {
    select() {
      listing++;
      if (!$('sftp-dock').hidden) list();
    },
    cancelTab(tab) {
      for (const task of tasks)
        if (task.tab === tab && !task.finished) {
          task.cancelled = true;
          task.controller.abort();
        }
    },
  };
}
