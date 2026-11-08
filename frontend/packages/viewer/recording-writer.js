// 专用Worker使用OPFS同步追加并flush，不反复复制增长中的整份录像。
// 本地恢复副本从不发送到WebService；页面崩溃后由下次打开页面恢复为一个MP4。
let access,
  fileName,
  offset = 0;
const directory = () =>
  navigator.storage
    .getDirectory()
    .then((root) => root.getDirectoryHandle('yoodesk-recordings', { create: true }));
self.onmessage = async ({ data: { id, command, name, bytes, position } }) => {
  try {
    const dir = await directory();
    let value;
    if (command === 'list') {
      value = [];
      for await (const [name, handle] of dir.entries()) {
        if (handle.kind !== 'file' || !name.endsWith('.mp4') || name === fileName) continue;
        let probe;
        try {
          // 活跃录制持有独占锁；其他标签不能恢复或删除正在写的录像。
          probe = await handle.createSyncAccessHandle();
          const size = probe.getSize();
          probe.close();
          probe = null;
          if (size) value.push({ name, size });
          else await dir.removeEntry(name);
        } catch (error) {
          probe?.close();
          if (error.name !== 'NoModificationAllowedError') throw error;
        }
      }
      value.sort((a, b) => b.name.localeCompare(a.name));
    } else if (command === 'open') {
      if (access) throw new Error('已有录制文件正在使用');
      const handle = await dir.getFileHandle(name, { create: true });
      access = await handle.createSyncAccessHandle();
      fileName = name;
      offset = access.getSize();
      value = offset;
    } else if (command === 'write') {
      if (!access) throw new Error('本地恢复文件未打开');
      const array = new Uint8Array(bytes);
      let written = 0;
      const previous = offset;
      try {
        while (written < array.length) {
          const count = access.write(array.subarray(written), { at: offset });
          if (!count) throw new Error('本地恢复文件写入中断');
          written += count;
          offset += count;
        }
        access.flush();
      } catch (error) {
        // 磁盘满等错误可能只写入半个容器片段，回退至最后完整片段后才允许恢复。
        access.truncate(previous);
        access.flush();
        offset = previous;
        throw error;
      }
      value = offset;
    } else if (command === 'read') {
      if (!access) throw new Error('本地恢复文件未打开');
      const array = new Uint8Array(Math.min(1024 * 1024, Math.max(0, access.getSize() - position)));
      const count = access.read(array, { at: position });
      value = array.buffer.slice(0, count);
    } else if (command === 'close' || command === 'remove') {
      access?.flush();
      access?.close();
      access = null;
      fileName = null;
      if (command === 'remove') await dir.removeEntry(name);
    } else throw new Error('未知录屏写入命令');
    self.postMessage({ id, value }, value instanceof ArrayBuffer ? [value] : []);
  } catch (error) {
    self.postMessage({ id, error: `${error.name}: ${error.message}` });
  }
};
