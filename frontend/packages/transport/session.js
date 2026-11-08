// 只续租原会话；调用者保留原session和requestID，响应丢失时可精确重试。
export class LeaseRenewalTimeout extends Error {
  constructor() {
    super('session renewal timed out');
    this.name = 'LeaseRenewalTimeout';
  }
}

export async function renewSession({
  origin,
  productID,
  session,
  csrfToken,
  requestID,
  control,
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
    !/^[A-Za-z0-9_-]{16,128}$/.test(session?.session_id) ||
    typeof session?.lease_version !== 'string' ||
    !/^[1-9][0-9]{0,18}$/.test(session?.lease_version) ||
    !Number.isFinite(Date.parse(session?.lease_expires_at)) ||
    typeof csrfToken !== 'string' ||
    !/^[A-Za-z0-9_-]{16,128}$/.test(csrfToken) ||
    typeof requestID !== 'string' ||
    !/^[A-Za-z0-9_-]{1,128}$/.test(requestID) ||
    typeof control?.waitForLease !== 'function' ||
    typeof fetchImpl !== 'function' ||
    !Number.isFinite(timeoutMs) ||
    timeoutMs <= 0 ||
    timeoutMs > 60000
  )
    throw new TypeError('explicit original session and renewal identity required');
  const original = { ...session };
  const abort = new AbortController();
  const stop = () => abort.abort(signal.reason);
  if (signal?.aborted) stop();
  signal?.addEventListener('abort', stop, { once: true });
  const timer = setTimeout(() => abort.abort(new LeaseRenewalTimeout()), timeoutMs);
  try {
    if (abort.signal.aborted) throw abort.signal.reason;
    const response = await fetchImpl(
      `${base.origin}/${productID}/api/v2/sessions/${original.session_id}/renew/`,
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
        body: JSON.stringify({ expected_lease_version: original.lease_version }),
        signal: abort.signal,
      },
    );
    if (!response.ok) throw new Error(`session renewal rejected (${response.status})`);
    const value = await response.json();
    const next = value.data;
    const fixed = [
      'session_id',
      'device_id',
      'capability',
      'target_id',
      'connection_generation',
      'negotiation_generation',
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
      typeof next.lease_version !== 'string' ||
      !/^[1-9][0-9]{0,18}$/.test(next.lease_version) ||
      BigInt(next.lease_version) !== BigInt(original.lease_version) + 1n ||
      typeof next.grant !== 'string' ||
      !next.grant ||
      !Number.isFinite(Date.parse(next.lease_expires_at)) ||
      Date.parse(next.lease_expires_at) <= Date.now() ||
      Date.parse(next.lease_expires_at) <= Date.parse(original.lease_expires_at)
    )
      throw new TypeError('renewal response changed original session scope');
    await control.waitForLease(next, { signal: abort.signal, timeoutMs });
    return Object.freeze({ ...next });
  } catch (error) {
    if (abort.signal.reason instanceof LeaseRenewalTimeout) throw abort.signal.reason;
    throw error;
  } finally {
    clearTimeout(timer);
    signal?.removeEventListener('abort', stop);
  }
}
