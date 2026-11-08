import { renewSession, LeaseRenewalTimeout } from './session.js';

const owners = new WeakSet();
const maxInterval = 5 * 60 * 1000;
const expiryMargin = 5000;

// 请求开始时间是周期锚点，响应延迟不能把下一次尝试推过节点流许可窗口。
// wall/mono仅在维护开始时映射一次，后续本机壁钟回拨不延长旧租约。
export function renewalDelay({
  expiresAt,
  anchorWall,
  anchorMono,
  attemptMono,
  nowMono,
  intervalMs = maxInterval,
}) {
  const expiry = Date.parse(expiresAt);
  if (
    !Number.isFinite(expiry) ||
    ![anchorWall, anchorMono, attemptMono, nowMono].every(Number.isFinite) ||
    !Number.isFinite(intervalMs) ||
    intervalMs < 1000 ||
    intervalMs > maxInterval
  )
    throw new RangeError('invalid bounded lease schedule');
  const deadline = anchorMono + expiry - anchorWall - expiryMargin;
  return Math.max(0, Math.min(attemptMono + intervalMs, deadline) - nowMono);
}

function wait(delay, signal) {
  if (signal.aborted) return Promise.reject(signal.reason);
  return new Promise((resolve, reject) => {
    const finish = (aborted, error) => {
      clearTimeout(timer);
      signal.removeEventListener('abort', abort);
      if (aborted) reject(error);
      else resolve();
    };
    const abort = () => finish(true, signal.reason);
    const timer = setTimeout(() => finish(false), delay);
    signal.addEventListener('abort', abort, { once: true });
  });
}

// 在业务连接ready后启动；一个控制实例只允许一个维护任务。
// onUpdate同步交付新DTO；普通取消/失败拒绝并关闭原控制实例。
// handoffSignal等待在途续租确认后返回最新DTO并保留控制实例；不延长授权。
// 只有明确续租超时可在原授权内按同requestID重试一次，不重放业务。
// 鉴权/协议拒绝及超出原有效期继续失败关闭，调用者须观察维护任务终止。
export async function maintainSessionLease({
  origin,
  productID,
  session,
  csrfToken,
  control,
  signal,
  handoffSignal,
  onUpdate,
  intervalMs = maxInterval,
  timeoutMs = 10000,
  fetchImpl = globalThis.fetch,
}) {
  if (
    typeof control?.waitForLease !== 'function' ||
    typeof control.onClose !== 'function' ||
    typeof control.close !== 'function' ||
    typeof fetchImpl !== 'function' ||
    (onUpdate !== undefined && typeof onUpdate !== 'function') ||
    (typeof csrfToken !== 'string' && typeof csrfToken !== 'function') ||
    !Number.isFinite(timeoutMs) ||
    timeoutMs <= 0 ||
    timeoutMs > 10000
  )
    throw new TypeError('explicit lease owner and bounded renewal required');
  const anchorWall = Date.now();
  const anchorMono = performance.now();
  let attemptMono = anchorMono;
  let current = Object.freeze({ ...session });
  renewalDelay({
    expiresAt: current.lease_expires_at,
    anchorWall,
    anchorMono,
    attemptMono,
    nowMono: anchorMono,
    intervalMs,
  });
  if (owners.has(control)) throw new Error('signaling lease already maintained');
  owners.add(control);
  const lifetime = new AbortController();
  const wake = new AbortController();
  const terminate = (reason) => {
    lifetime.abort(reason);
    wake.abort(reason);
  };
  const stop = () => terminate(signal.reason);
  let handoffRequested = false;
  let handedOff = false;
  const handoff = () => {
    handoffRequested = true;
    wake.abort(new Error('lease maintenance handoff requested'));
  };
  let unsubscribe = () => {};
  try {
    if (signal?.aborted) stop();
    signal?.addEventListener('abort', stop, { once: true });
    if (handoffSignal?.aborted) handoff();
    handoffSignal?.addEventListener('abort', handoff, { once: true });
    unsubscribe = control.onClose((error) => terminate(error ?? new Error('signaling closed')));
    if (typeof unsubscribe !== 'function') throw new TypeError('lease close subscription required');
    for (;;) {
      if (lifetime.signal.aborted) throw lifetime.signal.reason;
      if (handoffRequested) {
        handedOff = true;
        return current;
      }
      const delay = renewalDelay({
        expiresAt: current.lease_expires_at,
        anchorWall,
        anchorMono,
        attemptMono,
        nowMono: performance.now(),
        intervalMs,
      });
      try {
        await wait(delay, wake.signal);
      } catch (error) {
        if (lifetime.signal.aborted) throw lifetime.signal.reason;
        if (handoffRequested) {
          handedOff = true;
          return current;
        }
        throw error;
      }
      if (lifetime.signal.aborted) throw lifetime.signal.reason;
      if (handoffRequested) {
        handedOff = true;
        return current;
      }
      attemptMono = performance.now();
      const renewal = {
        origin,
        productID,
        session: current,
        csrfToken: typeof csrfToken === 'function' ? csrfToken() : csrfToken,
        requestID: crypto.randomUUID(),
        control,
        signal: lifetime.signal,
        timeoutMs,
        fetchImpl,
      };
      try {
        current = await renewSession(renewal);
      } catch (error) {
        const remaining =
          anchorMono +
          Date.parse(current.lease_expires_at) -
          anchorWall -
          performance.now() -
          expiryMargin;
        if (!(error instanceof LeaseRenewalTimeout) || lifetime.signal.aborted || remaining < 1000)
          throw error;
        // 保持原期望版本、CSRF和幂等编号；即使HTTP已提交，也只确认同一续租。
        current = await renewSession({ ...renewal, timeoutMs: Math.min(timeoutMs, remaining) });
      }
      onUpdate?.(current);
    }
  } finally {
    lifetime.abort(new Error('lease maintenance stopped'));
    signal?.removeEventListener('abort', stop);
    handoffSignal?.removeEventListener('abort', handoff);
    if (typeof unsubscribe === 'function') unsubscribe();
    owners.delete(control);
    if (!handedOff) control.close();
  }
}
