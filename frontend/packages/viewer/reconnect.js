// 单个逻辑会话只保留一个重试计时器；主动关闭撤销排队任务，退避最高15秒。
export function autoReconnect({
  connect,
  waiting = () => {},
  timer = setTimeout,
  clear = clearTimeout,
}) {
  let stopped = true,
    handle,
    attempt = 0,
    epoch = 0;
  function stop() {
    stopped = true;
    epoch++;
    clear(handle);
    handle = undefined;
  }
  function start() {
    if (stopped) attempt = 0;
    stopped = false;
  }
  function ready() {
    epoch++;
    clear(handle);
    handle = undefined;
    attempt = 0;
  }
  function retry() {
    if (stopped || handle !== undefined) return;
    const delay = Math.min(15000, 1000 * 2 ** Math.min(attempt++, 4));
    const current = epoch;
    waiting(delay);
    handle = timer(() => {
      if (stopped || epoch !== current) return;
      handle = undefined;
      void connect();
    }, delay);
  }
  return { start, stop, ready, retry };
}
