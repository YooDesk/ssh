export function recordingJournal() {
  const worker = new Worker(new URL('./recording-writer.js', import.meta.url), { type: 'module' });
  let next = 0;
  const pending = new Map();
  let failure;
  // Worker 失败或终止是终态；后续清理请求也必须立即失败，避免录屏一直停在保存中。
  const fail = (error) => {
    failure ||= error;
    for (const item of pending.values()) item.reject(failure);
    pending.clear();
  };
  worker.onmessage = ({ data }) => {
    const item = pending.get(data.id);
    if (!item) return;
    pending.delete(data.id);
    if (data.error) item.reject(new Error(data.error));
    else item.resolve(data.value);
  };
  worker.onerror = (event) => {
    fail(new Error(event.message || '本地录屏存储异常'));
  };
  return {
    request(command, options = {}) {
      return new Promise((resolve, reject) => {
        if (failure) {
          reject(failure);
          return;
        }
        const id = ++next;
        pending.set(id, { resolve, reject });
        try {
          worker.postMessage({ id, command, ...options }, options.bytes ? [options.bytes] : []);
        } catch (error) {
          pending.delete(id);
          reject(error);
        }
      });
    },
    terminate() {
      fail(new Error('本地录屏存储已关闭'));
      worker.terminate();
    },
  };
}

export function recordingRecovery(button, report) {
  const recovery = document.createElement('button');
  recovery.id = 'record-recover';
  recovery.textContent = '恢复录屏';
  recovery.hidden = true;
  recovery.title = '将浏览器本地保存的未完成录像恢复为单个MP4';
  button.after(recovery);
  let files = [],
    busy = false;
  const refresh = async () => {
    const journal = recordingJournal();
    try {
      files = await journal.request('list');
      recovery.hidden = !files.length;
      recovery.textContent = `恢复录屏 (${files.length})`;
    } catch (error) {
      report(`无法检查录屏恢复文件：${error.message}`);
    } finally {
      journal.terminate();
    }
  };
  recovery.onclick = () => {
    if (busy) return;
    const dialog = document.createElement('dialog'),
      heading = document.createElement('h2');
    heading.textContent = '恢复未完成的录屏';
    dialog.append(heading);
    for (const file of files) {
      const row = document.createElement('p'),
        save = document.createElement('button');
      save.textContent = `${file.name}（${(file.size / 1024 / 1024).toFixed(1)} MB）`;
      save.onclick = async () => {
        if (busy) return;
        busy = true;
        const journal = recordingJournal();
        let writer;
        try {
          // 选择器紧跟用户点击，保留浏览器要求的用户激活。
          const target = await showSaveFilePicker({
            suggestedName: file.name,
            types: [{ description: 'MP4 视频', accept: { 'video/mp4': ['.mp4'] } }],
          });
          const size = await journal.request('open', { name: file.name });
          writer = await target.createWritable();
          for (let position = 0; position < size;) {
            const bytes = await journal.request('read', { position });
            if (!bytes.byteLength) throw new Error('恢复文件读取意外结束');
            await writer.write(bytes);
            position += bytes.byteLength;
          }
          await writer.close();
          writer = null;
          await journal.request('remove', { name: file.name });
          report(`已恢复录屏：${target.name}。末尾尚未落盘的画面可能缺失。`);
          dialog.close();
          await refresh();
        } catch (error) {
          if (writer) await writer.abort().catch(() => {});
          if (error.name !== 'AbortError')
            report(`恢复失败：${error.message}；本地恢复副本仍保留。`);
        } finally {
          journal.terminate();
          busy = false;
        }
      };
      row.append(save);
      dialog.append(row);
    }
    const close = document.createElement('button');
    close.textContent = '关闭';
    close.onclick = () => dialog.close();
    dialog.append(close);
    dialog.onclose = () => dialog.remove();
    document.body.append(dialog);
    dialog.showModal();
  };
  return refresh;
}
