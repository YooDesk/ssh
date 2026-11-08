import { $ } from './access.js';

// MS-RDPECLIP文件通道：上传按远端请求读取本地File切片，下载一次只请求一个64KiB块。
export function rdpFiles({
  session,
  engine,
  enabled,
  report,
  acceptance,
  focused = () => true,
  readLocal,
  pasteAction,
  copyAction,
}) {
  let epoch = 0,
    uploads = [],
    remoteFiles = [],
    clipId,
    download,
    stream = 0,
    readers = 0,
    uploadUntil = 0,
    uploadReady = false;
  const requests = new Map();
  const locked = new Map();
  const uploadRequests = new Map();
  let localBaseline, localRead;
  function trackLocal() {
    localBaseline = undefined;
    if (!readLocal) return;
    const current = epoch;
    Promise.resolve()
      .then(readLocal)
      .then((text) => {
        if (current === epoch) localBaseline = text;
      })
      .catch(() => {});
  }
  async function checkLocal() {
    if (
      !readLocal ||
      localRead ||
      localBaseline === undefined ||
      !enabled() ||
      !focused() ||
      download ||
      (!uploads.length && !remoteFiles.length)
    )
      return;
    const current = epoch;
    const work = Promise.resolve()
      .then(readLocal)
      .then((text) => {
        if (current !== epoch || !enabled() || !focused() || text === localBaseline) return;
        uploads = [];
        remoteFiles = [];
        uploadUntil = 0;
        localBaseline = undefined;
        render();
      })
      .catch(() => {});
    localRead = work;
    try {
      await work;
    } finally {
      if (localRead === work) localRead = undefined;
    }
  }
  const invoke = (name, value) => session().invokeExtension(new (engine().Extension)(name, value));
  const safeName = (name) =>
    String(name)
      .split(/[\\/]/)
      .at(-1)
      .replace(/[\x00-\x1f<>:"|?*]/g, '_') || 'download';
  const state = (text, error = false) => {
    $('rdp-file-status').textContent = text;
    $('rdp-file-status').className = error ? 'file-status error' : 'file-status';
  };
  function reset() {
    for (const [id, target] of uploadRequests) {
      if (target && target === session())
        invoke('submit_file_contents', { stream_id: id, is_error: true, data: new Uint8Array() });
    }
    uploadRequests.clear();
    epoch++;
    localBaseline = undefined;
    uploads = [];
    remoteFiles = [];
    clipId = undefined;
    uploadUntil = 0;
    uploadReady = false;
    if (download) download.cancelled = true;
    for (const item of requests.values()) {
      clearTimeout(item.timer);
      item.reject(Error('传输已取消'));
    }
    requests.clear();
    locked.clear();
    render();
    state('连接后可传输文件');
    $('rdp-file-upload').value = '';
  }
  function render() {
    $('rdp-file-upload').disabled = !enabled();
    $('rdp-file-paste').disabled = !enabled() || !uploadReady || !uploads.length;
    $('rdp-file-copy').disabled = !enabled();
    $('rdp-file-empty').hidden = remoteFiles.length > 0;
    $('rdp-file-selection').replaceChildren(
      ...uploads.map((file) => {
        const li = document.createElement('li');
        li.className = 'file-row';
        li.textContent = `${file.name} · ${file.size.toLocaleString()} B`;
        return li;
      }),
    );
    $('rdp-remote-files').replaceChildren(
      ...remoteFiles.map((file, index) => {
        const li = document.createElement('li'),
          label = document.createElement('span'),
          button = document.createElement('button');
        li.className = 'file-row';
        label.className = 'file-name';
        label.textContent = file.isDirectory
          ? `${file.name}（文件夹）`
          : `${file.name} (${file.size.toLocaleString()} B)`;
        button.textContent = '下载';
        button.type = 'button';
        button.title = file.isDirectory ? '请进入文件夹后选择文件下载' : '下载文件';
        button.disabled = file.isDirectory || !enabled() || !!download;
        button.onclick = () => void save(file, index).catch((error) => state(error.message));
        li.append(label, button);
        return li;
      }),
    );
  }
  function available(files, id) {
    if (!enabled()) return;
    if (!Array.isArray(files) || files.length > 256) {
      report('一次最多接收 256 个文件');
      return;
    }
    remoteFiles = files.map((file) => ({ ...file, name: safeName(file.name) }));
    uploads = [];
    uploadUntil = 0;
    uploadReady = false;
    trackLocal();
    clipId = id;
    render();
    if (files.length) state('已收到远端复制的文件，点击下载');
  }
  async function requestUpload(request) {
    const current = epoch,
      target = session();
    const fail = () => {
      if (target && target === session())
        invoke('submit_file_contents', {
          stream_id: request.streamId,
          is_error: true,
          data: new Uint8Array(),
        });
    };
    const source = request.dataId === undefined ? uploads : locked.get(request.dataId);
    if (
      !enabled() ||
      readers >= 4 ||
      !source?.[request.index] ||
      !Number.isSafeInteger(request.position) ||
      request.position < 0 ||
      !Number.isSafeInteger(request.size) ||
      request.size < 0 ||
      request.size > 1048576
    ) {
      fail();
      return;
    }
    readers++;
    uploadRequests.set(request.streamId, target);
    try {
      const file = source[request.index];
      let data;
      if (request.flags === 1) {
        data = new Uint8Array(8);
        new DataView(data.buffer).setBigUint64(0, BigInt(file.size), true);
      } else if (request.flags === 2) {
        data = new Uint8Array(
          await file.slice(request.position, request.position + request.size).arrayBuffer(),
        );
      } else {
        fail();
        return;
      }
      if (current !== epoch || target !== session() || !enabled()) return;
      invoke('submit_file_contents', { stream_id: request.streamId, is_error: false, data });
      if (request.flags === 2)
        state(
          `正在发送 ${file.name}：${Math.min(file.size, request.position + data.length)} / ${file.size} B`,
        );
    } catch (error) {
      fail();
      state(error.message);
    } finally {
      uploadRequests.delete(request.streamId);
      readers--;
    }
  }
  function response(message) {
    const item = requests.get(message.streamId);
    if (!item) return;
    requests.delete(message.streamId);
    clearTimeout(item.timer);
    if (message.isError) item.reject(Error('远端拒绝读取文件，请重新复制后再试'));
    else item.resolve(new Uint8Array(message.data));
  }
  function readChunk(index, position, size, sourceId) {
    if (requests.size >= 1) return Promise.reject(Error('已有文件读取请求'));
    return new Promise((resolve, reject) => {
      const id = ++stream;
      const timer = setTimeout(() => {
        requests.delete(id);
        reject(Error('文件读取超时'));
      }, 15000);
      requests.set(id, { resolve, reject, timer });
      try {
        invoke('request_file_contents', {
          stream_id: id,
          file_index: index,
          flags: 2,
          position,
          size,
          ...(sourceId === undefined ? {} : { clip_data_id: sourceId }),
        });
      } catch (error) {
        clearTimeout(timer);
        requests.delete(id);
        reject(error);
      }
    });
  }
  async function save(file, index) {
    if (download || !enabled()) return;
    if (!Number.isSafeInteger(file.size) || file.size < 0) throw Error('文件大小无效');
    const task = { cancelled: false, sourceId: clipId },
      current = epoch,
      parts = [];
    let writer;
    download = task;
    render();
    try {
      if (file.size > 64 * 1048576) {
        if (!window.showSaveFilePicker) throw Error('大于 64 MiB 的文件需要支持直接保存的浏览器');
        writer = await (
          await window.showSaveFilePicker({ suggestedName: file.name })
        ).createWritable();
      }
      let offset = 0;
      while (offset < file.size) {
        if (task.cancelled || current !== epoch || !enabled()) throw Error('传输已取消');
        const size = Math.min(65536, file.size - offset),
          data = await readChunk(index, offset, size, task.sourceId);
        if (data.length === 0 || data.length > size) throw Error('远端返回的文件长度无效');
        if (task.cancelled || current !== epoch || !enabled()) throw Error('传输已取消');
        if (writer) await writer.write(data);
        else parts.push(data);
        offset += data.length;
        state(`正在下载 ${file.name}：${offset} / ${file.size} B`);
      }
      if (task.cancelled || current !== epoch || !enabled()) throw Error('传输已取消');
      if (writer) await writer.close();
      else {
        const url = URL.createObjectURL(new Blob(parts));
        const a = document.createElement('a');
        a.href = url;
        a.download = file.name;
        a.click();
        setTimeout(() => URL.revokeObjectURL(url), 1000);
      }
      state(`已下载 ${file.name}`);
    } catch (error) {
      await writer?.abort().catch(() => {});
      throw error;
    } finally {
      if (download === task) download = null;
      render();
    }
  }
  $('rdp-file-upload').onchange = () => {
    if (!enabled()) return;
    const files = [...$('rdp-file-upload').files];
    if (!files.length) {
      uploads = [];
      uploadUntil = 0;
      uploadReady = false;
      render();
      state('未选择文件');
      return;
    }
    if (files.length > 64) {
      uploads = [];
      uploadUntil = 0;
      uploadReady = false;
      render();
      state('一次最多选择 64 个文件', true);
      return;
    }
    if (files.some((file) => file.name.length > 259 || /[\x00-\x1f<>:"\\/|?*]/.test(file.name))) {
      uploads = [];
      uploadUntil = 0;
      uploadReady = false;
      render();
      state('所选文件名不符合 Windows 命名要求', true);
      return;
    }
    uploads = files;
    trackLocal();
    uploadReady = false;
    uploadUntil = Infinity;
    render();
    try {
      const accepted = acceptance(),
        current = epoch;
      invoke(
        'initiate_file_copy',
        files.map((file) => ({
          name: file.name,
          size: file.size,
          lastModified: file.lastModified,
          isDirectory: false,
        })),
      );
      state('正在准备文件复制');
      accepted.then(
        () => {
          if (current === epoch && uploads === files) formatReady(true);
        },
        (error) => {
          if (current === epoch && uploads === files) {
            uploads = [];
            uploadUntil = 0;
            uploadReady = false;
            render();
            state(`未能准备文件复制：${error.message}`, true);
          }
        },
      );
    } catch (error) {
      uploads = [];
      uploadUntil = 0;
      state(error.message);
    }
  };
  $('rdp-file-cancel').onclick = () => {
    reset();
    state('已取消传输');
  };
  $('rdp-file-paste').onclick = () => {
    if (enabled() && uploadReady) pasteAction?.();
  };
  $('rdp-file-copy').onclick = () => {
    if (enabled()) {
      state('正在读取远端选中的文件…');
      copyAction?.();
    }
  };
  $('rdp-file-drop').ondragover = (event) => {
    if (enabled()) event.preventDefault();
  };
  $('rdp-file-drop').ondrop = (event) => {
    event.preventDefault();
    if (enabled()) {
      $('rdp-file-upload').files = event.dataTransfer.files;
      $('rdp-file-upload').onchange();
    }
  };
  $('files-toggle').onclick = () => {
    $('rdp-files').hidden = !$('rdp-files').hidden;
    render();
  };
  $('rdp-files-close').onclick = () => {
    $('rdp-files').hidden = true;
  };
  function lock(id) {
    if (locked.size < 8) locked.set(id, [...uploads]);
  }
  function unlock(id) {
    locked.delete(id);
  }
  function expired(ids) {
    const values = Array.from(ids);
    if (download && values.includes(download.sourceId)) {
      download.cancelled = true;
      for (const item of requests.values()) {
        clearTimeout(item.timer);
        item.reject(Error('远端文件复制已过期，请重新复制后下载'));
      }
      requests.clear();
    }
    if (values.includes(clipId)) {
      remoteFiles = [];
      clipId = undefined;
      render();
      if (!uploads.length) state('远端文件复制已过期，请重新选中文件并复制');
    }
  }
  function textChanged() {
    if (!download) {
      uploads = [];
      uploadUntil = 0;
      localBaseline = undefined;
      remoteFiles = [];
      render();
    }
  }
  function formatReady(ok) {
    if (!uploads.length) return;
    uploadReady = ok;
    render();
    state(ok ? '文件已就绪，点击“粘贴文件到远端”，或在远端按 Ctrl+V' : '远端拒绝文件复制', !ok);
  }
  async function paste(keys, textPaste) {
    await checkLocal();
    if (!uploads.length && !remoteFiles.length) {
      await textPaste?.();
      return;
    }
    const current = epoch;
    for (let n = 0; uploads.length && !uploadReady && n < 150; n++) {
      await new Promise((resolve) => setTimeout(resolve, 100));
      if (current !== epoch || !enabled() || !focused()) return;
    }
    if (current !== epoch || !enabled() || !focused()) return;
    if (uploads.length && !uploadReady) {
      state('远端尚未接受文件复制，请重新选择文件');
      return;
    }
    keys();
  }
  return {
    ready: () => {
      render();
      state('文件通道已就绪，可选择上传文件或复制远端文件下载');
    },
    reset,
    available,
    requestUpload,
    response,
    lock,
    unlock,
    expired,
    textChanged,
    checkLocal,
    formatReady,
    paste,
    holdsClipboard: () => !!download || remoteFiles.length > 0 || Date.now() < uploadUntil,
  };
}
