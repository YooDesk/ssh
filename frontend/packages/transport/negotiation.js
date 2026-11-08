// 调用者保留原session/path/requestID，响应丢失后仅精确重试，不重放业务。
export async function negotiateSession({
  origin,
  productID,
  session,
  path,
  csrfToken,
  requestID,
  signal,
  timeoutMs = 15000,
  fetchImpl = globalThis.fetch,
}) {
  const base = new URL(origin);
  if (
    base.protocol !== 'https:' ||
    base.href !== base.origin + '/' ||
    (globalThis.location && base.origin !== globalThis.location.origin) ||
    !/^[a-z][a-z0-9-]{0,31}$/.test(productID) ||
    typeof session?.session_id !== 'string' ||
    !/^[A-Za-z0-9_-]{16,128}$/.test(session.session_id) ||
    typeof session.negotiation_generation !== 'string' ||
    !/^[1-9][0-9]{0,19}$/.test(session.negotiation_generation) ||
    typeof session.lease_version !== 'string' ||
    !/^[1-9][0-9]{0,18}$/.test(session.lease_version) ||
    !['stun', 'turn', 'websocket'].includes(path) ||
    typeof csrfToken !== 'string' ||
    !/^[A-Za-z0-9_-]{16,128}$/.test(csrfToken) ||
    typeof requestID !== 'string' ||
    !/^[A-Za-z0-9_-]{1,128}$/.test(requestID) ||
    typeof fetchImpl !== 'function' ||
    !Number.isFinite(timeoutMs) ||
    timeoutMs <= 0 ||
    timeoutMs > 60000
  )
    throw new TypeError('explicit original session and negotiation identity required');
  const original = { ...session };
  const abort = new AbortController();
  const stop = () => abort.abort(signal.reason);
  if (signal?.aborted) stop();
  signal?.addEventListener('abort', stop, { once: true });
  const timer = setTimeout(
    () => abort.abort(new Error('session negotiation timed out')),
    timeoutMs,
  );
  try {
    if (abort.signal.aborted) throw abort.signal.reason;
    const response = await fetchImpl(
      `${base.origin}/${productID}/api/v2/sessions/${original.session_id}/negotiate/`,
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
        body: JSON.stringify({
          expected_negotiation_generation: original.negotiation_generation,
          path,
        }),
        signal: abort.signal,
      },
    );
    // 授权/候选错误不是ConnectivityError，选择器不得据此继续试其他权限范围。
    if (!response.ok) throw new Error(`session negotiation rejected (${response.status})`);
    const value = await response.json();
    const next = value.data;
    const fixed = [
      'session_id',
      'device_id',
      'capability',
      'target_id',
      'connection_generation',
      'policy_revision',
      'signal_url',
      'relay_url',
      'device_origin',
      'frontend_mode',
    ];
    if (
      value.v !== 2 ||
      value.request_id !== requestID ||
      !next ||
      fixed.some((key) => typeof original[key] !== 'string' || next[key] !== original[key]) ||
      typeof next.negotiation_generation !== 'string' ||
      !/^[1-9][0-9]{0,19}$/.test(next.negotiation_generation) ||
      BigInt(next.negotiation_generation) <= BigInt(original.negotiation_generation) ||
      typeof next.lease_version !== 'string' ||
      !/^[1-9][0-9]{0,18}$/.test(next.lease_version) ||
      BigInt(next.lease_version) <= BigInt(original.lease_version) ||
      typeof next.grant !== 'string' ||
      !next.grant ||
      !Number.isFinite(Date.parse(next.lease_expires_at)) ||
      Date.parse(next.lease_expires_at) <= Date.now()
    )
      throw new TypeError('negotiation response changed original session scope or generation');
    return Object.freeze({ ...next });
  } finally {
    clearTimeout(timer);
    signal?.removeEventListener('abort', stop);
  }
}
