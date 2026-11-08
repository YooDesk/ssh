// 原生viewer共用自动文本剪贴板；所有异步结果绑定连接、模式与远端复制代次。
export function textClipboard({
  session,
  enabled,
  focused,
  send,
  report,
  clipboard = navigator.clipboard,
}) {
  let epoch = 0,
    token,
    known,
    pending,
    version = 0,
    busy,
    readDenied = false,
    writeDenied = false,
    pasting = false,
    lastError = '',
    pauseUntil = 0;
  function reset() {
    epoch++;
    token = session();
    known = undefined;
    pending = undefined;
    readDenied = writeDenied = false;
    pasting = false;
    lastError = '';
    pauseUntil = 0;
  }
  function current() {
    if (token !== session()) reset();
    return epoch;
  }
  const valid = (e) => e === epoch && token === session() && !!token && enabled() && focused();
  function error(e) {
    const text = `请在浏览器中允许剪贴板同步：${e.message}`;
    if (text !== lastError) {
      lastError = text;
      report(text);
    }
  }
  function check(text) {
    if (typeof text !== 'string' || new TextEncoder().encode(text).length > 1048576)
      throw Error('剪贴板文本不能超过 1 MiB');
  }
  async function cycle(e, force = false, text) {
    if (!valid(e)) return false;
    if (pending && !force) {
      const item = pending;
      await clipboard.writeText(item.text);
      if (!valid(e)) return false;
      known = item.text;
      if (pending === item) pending = undefined;
      return true;
    }
    if (!force && (readDenied || Date.now() < pauseUntil)) return false;
    const seen = version;
    const value = text === undefined ? await clipboard.readText() : text;
    if (!valid(e) || seen !== version) return false;
    check(value);
    if (force || (value !== known && !(known === undefined && value === ''))) {
      await send(value);
      if (!valid(e) || seen !== version) return false;
      pending = undefined;
    }
    known = value;
    return true;
  }
  function sync() {
    const e = current();
    if (busy || !valid(e) || (pending ? writeDenied : readDenied))
      return busy || Promise.resolve(false);
    const writing = !!pending;
    const work = cycle(e).catch((err) => {
      if (valid(e)) {
        if (writing) writeDenied = true;
        else readDenied = true;
        error(err);
      }
      return false;
    });
    busy = work;
    work.finally(() => {
      if (busy === work) busy = undefined;
    });
    return work;
  }
  async function paste(keys, text) {
    const e = current();
    if (pasting || !valid(e)) return;
    pasting = true;
    const previous = busy;
    if (previous) await previous;
    if (!valid(e)) {
      if (e === epoch) pasting = false;
      return;
    }
    // 一次显式粘贴必须先完成剪贴板传输；失败不能粘贴旧远端内容。
    const work = cycle(e, true, text)
      .then((ok) => {
        if (ok && valid(e)) keys();
      })
      .catch((err) => {
        if (valid(e)) error(err);
      });
    busy = work;
    try {
      await work;
    } finally {
      if (busy === work) busy = undefined;
      if (e === epoch) pasting = false;
    }
  }
  function receive(text) {
    current();
    if (!token || !enabled()) return;
    try {
      check(text);
    } catch (err) {
      error(err);
      return;
    }
    version++;
    pending = { text };
    pauseUntil = 0;
    void sync();
  }
  function wake() {
    readDenied = writeDenied = false;
    void sync();
  }
  function copiedRemote() {
    version++;
    pauseUntil = Date.now() + 1500;
  }
  return { sync, paste, receive, reset, wake, copiedRemote };
}
