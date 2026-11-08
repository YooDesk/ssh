// 清理失败不能覆盖连接首错；同时保留两者并停止后续选路。
export async function disposeAfterFailure(prepared, error, options) {
  try {
    await prepared.dispose?.(options);
  } catch (cleanupError) {
    throw new AggregateError(
      [error, cleanupError],
      `${error.message}; cleanup failed: ${cleanupError.message}`,
      { cause: error },
    );
  }
}

// 释放原代次；用户明确撤销整个SID应使用DELETE，不能用旧连接dispose代替该意图。
export async function releaseSession({
  origin,
  productID,
  session,
  csrfToken,
  requestID,
  signal,
  timeoutMs = 5000,
  fetchImpl = globalThis.fetch,
}) {
  const original = { ...session };
  const base = new URL(origin);
  if (
    base.protocol !== 'https:' ||
    base.href !== base.origin + '/' ||
    (globalThis.location && base.origin !== globalThis.location.origin) ||
    typeof productID !== 'string' ||
    !/^[a-z][a-z0-9-]{0,31}$/.test(productID) ||
    typeof original.session_id !== 'string' ||
    !/^[A-Za-z0-9_-]{16,128}$/.test(original.session_id) ||
    typeof original.negotiation_generation !== 'string' ||
    !/^[1-9][0-9]{0,19}$/.test(original.negotiation_generation) ||
    typeof csrfToken !== 'string' ||
    !/^[A-Za-z0-9_-]{16,128}$/.test(csrfToken) ||
    typeof requestID !== 'string' ||
    !/^[A-Za-z0-9_-]{1,128}$/.test(requestID) ||
    typeof fetchImpl !== 'function' ||
    !Number.isFinite(timeoutMs) ||
    timeoutMs <= 0 ||
    timeoutMs > 5000
  )
    throw new TypeError('explicit original generation cleanup required');
  const abort = new AbortController();
  const stop = () => abort.abort(signal.reason);
  if (signal?.aborted) stop();
  signal?.addEventListener('abort', stop, { once: true });
  const timer = setTimeout(() => abort.abort(new Error('session release timed out')), timeoutMs);
  try {
    if (abort.signal.aborted) throw abort.signal.reason;
    const response = await fetchImpl(
      `${base.origin}/${productID}/api/v2/sessions/${original.session_id}/release/`,
      {
        method: 'POST',
        credentials: 'same-origin',
        redirect: 'error',
        cache: 'no-store',
        headers: {
          'Content-Type': 'application/json',
          'X-CSRF-Token': csrfToken,
          'X-Request-ID': requestID,
        },
        body: JSON.stringify({ expected_negotiation_generation: original.negotiation_generation }),
        signal: abort.signal,
      },
    );
    if (response.status !== 204)
      throw new Error(`session generation release rejected (${response.status})`);
  } finally {
    clearTimeout(timer);
    signal?.removeEventListener('abort', stop);
  }
}
