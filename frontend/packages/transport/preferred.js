import { connectRTC, ConnectivityError } from './rtc.js';
import { connectRelay } from './relay.js';
import { disposeAfterFailure } from './release.js';

const identityKeys = ['origin', 'pathPrefix', 'sessionID', 'capability', 'targetID'];
const generation = (value) => {
  if (typeof value !== 'string' || !/^[1-9][0-9]{0,19}$/.test(value))
    throw new TypeError('invalid negotiation generation');
  return BigInt(value);
};

function withinSignal(operation, signal, late) {
  signal.throwIfAborted();
  return new Promise((resolve, reject) => {
    const abort = () => reject(signal.reason);
    signal.addEventListener('abort', abort, { once: true });
    Promise.resolve()
      .then(() => {
        signal.throwIfAborted();
        return operation();
      })
      .then((value) => {
        if (signal.aborted) {
          if (late)
            void Promise.resolve()
              .then(() => late(value))
              .catch(() => {});
        } else {
          resolve(value);
        }
      }, reject)
      .finally(() => signal.removeEventListener('abort', abort));
  });
}

// prepare 向权威控制面申请该阶段的新 grant/ICE/代次；不能复用旧协商响应。
// dial 是传输适配接口，默认使用本 SDK 已验证的真实 RTC/WSS 实现。
export async function connectPreferred({
  context,
  policy,
  prepare,
  timeoutMs = 15000,
  dial,
  requestedPath,
  signal,
}) {
  if (
    typeof prepare !== 'function' ||
    typeof policy?.allowTURN !== 'boolean' ||
    typeof policy?.allowWSS !== 'boolean'
  )
    throw new TypeError('explicit session policy and prepare callback required');
  if (!Number.isFinite(timeoutMs) || timeoutMs <= 0 || timeoutMs > 60000)
    throw new RangeError('invalid transport selection deadline');
  let paths = ['stun'];
  if (policy.allowTURN) paths.push('turn');
  if (policy.allowWSS) paths.push('websocket');
  // 显式诊断只尝试获准的指定链路，失败不能自动切换后被误计为该链路成功。
  if (requestedPath !== undefined) {
    if (!paths.includes(requestedPath))
      throw new TypeError('requested transport unavailable by policy');
    paths = [requestedPath];
  }
  const connect =
    dial ??
    ((path, options) => (path === 'websocket' ? connectRelay(options) : connectRTC(options)));
  let last = generation(context.generation) - 1n;
  const attempts = [];
  const errors = [];
  const deadline = performance.now() + timeoutMs;
  for (const [pathIndex, path] of paths.entries()) {
    signal?.throwIfAborted();
    const left = deadline - performance.now();
    if (left < 1) break;
    const budget = Math.max(1, Math.floor(left / (paths.length - pathIndex)));
    const stageDeadline = performance.now() + budget;
    const remaining = () => Math.max(1, Math.ceil(stageDeadline - performance.now()));
    const stageTimeout = AbortSignal.timeout(budget);
    const stageSignal = signal ? AbortSignal.any([signal, stageTimeout]) : stageTimeout;
    let prepared;
    try {
      prepared = await withinSignal(
        () => prepare(path, { signal: stageSignal, remaining }),
        stageSignal,
        (late) => late.dispose?.({ retrying: false }),
      );
    } catch (error) {
      signal?.throwIfAborted();
      if (stageTimeout.aborted && error === stageSignal.reason)
        error = new ConnectivityError('transport preparation deadline exceeded', { cause: error });
      if (!(error instanceof ConnectivityError)) throw error;
      attempts.push({ path, outcome: 'unavailable' });
      errors.push(error);
      continue;
    }
    const next = prepared.context;
    try {
      if (
        !identityKeys.every((key) => next?.[key] === context[key]) ||
        generation(next.generation) <= last
      )
        throw new Error('session scope or negotiation generation changed unexpectedly');
      last = generation(next.generation);
      if (
        path === 'stun' &&
        (prepared.configuration?.iceTransportPolicy === 'relay' ||
          prepared.configuration?.iceServers?.some((server) =>
            server.urls.some((url) => url.startsWith('turn:') || url.startsWith('turns:')),
          ))
      )
        throw new Error('direct stage must use approved STUN configuration only');
      if (path === 'turn' && prepared.configuration?.iceTransportPolicy !== 'relay')
        throw new Error('TURN stage requires relay policy');
    } catch (error) {
      await disposeAfterFailure(prepared, error, { retrying: false });
      throw error;
    }
    let link;
    try {
      if (stageSignal.aborted) throw new ConnectivityError('transport stage deadline exceeded');
      link = await withinSignal(
        () => connect(path, { ...prepared, timeoutMs: remaining(), signal: stageSignal }),
        stageSignal,
        (late) => late.close(),
      );
      const pair = await withinSignal(() => link.selectedPair(), stageSignal);
      signal?.throwIfAborted();
      if (stageTimeout.aborted) throw new ConnectivityError('transport stage deadline exceeded');
      if (path === 'stun' && (pair.local === 'relay' || pair.remote === 'relay'))
        throw new Error('unexpected relay on direct stage');
      if (path === 'turn' && (pair.local !== 'relay' || pair.remote !== 'relay'))
        throw new Error('TURN selected pair violates relay policy');
      if (path === 'websocket' && pair.local !== 'websocket')
        throw new Error('WSS path not established');
      attempts.push({ path, outcome: 'connected' });
      let closed = false;
      return {
        ...link,
        path,
        attempts,
        selectedPair: async () => pair,
        async close() {
          if (closed) return;
          closed = true;
          try {
            await link.close();
          } finally {
            await prepared.dispose?.({ retrying: false });
          }
        },
      };
    } catch (error) {
      if (stageTimeout.aborted && !signal?.aborted)
        error = new ConnectivityError('transport stage deadline exceeded', { cause: error });
      if (link) await link.close();
      // 下一代提交前保留旧信令所有权，避免提前 session_close 产生终态墓碑。
      await disposeAfterFailure(prepared, error, {
        retrying: error instanceof ConnectivityError && pathIndex + 1 < paths.length,
      });
      if (!(error instanceof ConnectivityError)) throw error;
      attempts.push({ path, outcome: 'unavailable' });
      errors.push(error);
    }
  }
  throw new AggregateError(errors, 'approved transport paths unavailable');
}
