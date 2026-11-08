import {
  encodeControl,
  decodeControl,
  checkControlDirection,
  checkControlBinding,
  StaleControlError,
} from './control.js';

// socket 必须由已认证的接入流程建立；SDK 不拼接账号凭据到 URL。
export class SignalControl {
  #socket;
  #binding;
  #pending = new Map();
  #listeners = new Set();
  #candidates = [];
  #candidateCount = 0;
  #closed;
  #receive;
  #disconnect;
  #lease;
  #closeListeners = new Set();
  #leaseWaiters = new Set();
  #ice;
  #iceIdentity;
  #iceWaiters = new Set();

  constructor(socket, { connectionGeneration, sessionID, negotiationGeneration }) {
    if (socket?.protocol !== 'raccess.signal.v2' || socket.readyState !== 1)
      throw new TypeError('open v2 signaling WebSocket required');
    if (
      !/^[1-9][0-9]{0,19}$/.test(connectionGeneration) ||
      !/^[1-9][0-9]{0,19}$/.test(negotiationGeneration) ||
      typeof sessionID !== 'string' ||
      !sessionID ||
      [...sessionID].length > 128
    )
      throw new TypeError('explicit signaling identity and generations required');
    this.#socket = socket;
    this.#binding = { connectionGeneration, sessionID, negotiationGeneration };
    this.#receive = (event) => {
      try {
        this.#message(event.data);
      } catch (error) {
        // 浏览器 close() 仅允许 1000 或 3000..4999；协议/策略错误使用应用关闭码。
        this.#stop(error, error instanceof StaleControlError ? 4008 : 4002);
      }
    };
    this.#disconnect = () => this.#stop(new Error('signaling connection closed'), 1000);
    socket.addEventListener('message', this.#receive);
    socket.addEventListener('close', this.#disconnect);
    socket.addEventListener('error', this.#disconnect);
  }

  #stop(error, code) {
    if (this.#closed) return;
    this.#closed = error;
    this.#socket.removeEventListener('message', this.#receive);
    this.#socket.removeEventListener('close', this.#disconnect);
    this.#socket.removeEventListener('error', this.#disconnect);
    for (const pending of [...this.#pending.values()]) pending.finish(error);
    for (const pending of [...this.#leaseWaiters]) pending.finish(error);
    for (const pending of [...this.#iceWaiters]) pending.finish(error);
    this.#listeners.clear();
    this.#candidates.length = 0;
    this.#ice = undefined;
    this.#iceIdentity = undefined;
    this.#socket.close(code, 'signaling terminated');
    const listeners = [...this.#closeListeners];
    this.#closeListeners.clear();
    for (const listener of listeners) listener(error);
  }

  #message(text) {
    const message = decodeControl(text);
    checkControlDirection(message, 'gateway');
    const negotiated = ['answer', 'ice_candidate', 'relay_ready', 'ice_config_update'].includes(
      message.type,
    );
    checkControlBinding(message, {
      ...this.#binding,
      negotiationGeneration: negotiated ? this.#binding.negotiationGeneration : undefined,
    });
    if (message.type === 'ice_config_update') {
      const ice = message.payload.ice_config;
      if (Date.parse(ice.expires_at) <= Date.now())
        throw new StaleControlError('expired ICE snapshot');
      const identity = JSON.stringify([
        ice.revision,
        ice.policy_revision,
        ice.expires_at,
        ice.ice_transport_policy,
        ice.allow_wss,
        ice.ice_servers.map((server) => [
          server.urls,
          server.username,
          server.credential,
          server.credential_expires_at,
        ]),
      ]);
      if (this.#ice && this.#iceIdentity !== identity)
        throw new StaleControlError('conflicting ICE snapshot for original negotiation');
      this.#iceIdentity = identity;
      this.#ice = Object.freeze({
        ...ice,
        ice_servers: Object.freeze(
          ice.ice_servers.map((server) =>
            Object.freeze({
              ...server,
              urls: Object.freeze([...server.urls]),
            }),
          ),
        ),
      });
      for (const pending of [...this.#iceWaiters]) pending.finish();
      return;
    }
    if (message.type === 'ice_candidate') {
      if (++this.#candidateCount > 64) throw new RangeError('candidate limit exceeded');
      if (!this.#listeners.size) this.#candidates.push(message.payload.candidate);
      else for (const listener of this.#listeners) listener(message.payload.candidate);
      return;
    }
    if (message.type === 'lease_update') {
      const next = message.payload;
      const expiry = Date.parse(next.lease_expires_at);
      if (!Number.isFinite(expiry) || expiry <= Date.now())
        throw new TypeError('expired lease update');
      if (this.#lease) {
        const old = this.#lease;
        const newer =
          next.lease_version.length > old.lease_version.length ||
          (next.lease_version.length === old.lease_version.length &&
            next.lease_version > old.lease_version);
        if (
          next.lease_version === old.lease_version &&
          next.grant === old.grant &&
          next.lease_expires_at === old.lease_expires_at
        )
          return;
        if (!newer || expiry <= Date.parse(old.lease_expires_at))
          throw new StaleControlError('stale lease update');
      }
      this.#lease = Object.freeze({ ...next });
      for (const pending of [...this.#leaseWaiters]) {
        if (BigInt(next.lease_version) < BigInt(pending.lease.lease_version)) continue;
        pending.finish(
          next.grant === pending.lease.grant &&
            next.lease_version === pending.lease.lease_version &&
            Date.parse(next.lease_expires_at) === Date.parse(pending.lease.lease_expires_at)
            ? undefined
            : new StaleControlError('applied lease differs from renewal response'),
        );
      }
      return;
    }
    if (message.type === 'error' || message.type === 'session_close')
      throw new Error('remote signaling terminated');
    if (!['answer', 'relay_ready'].includes(message.type))
      throw new TypeError('unsupported browser signaling message');
    const pending = this.#pending.get(message.request_id);
    if (!pending) return; // 已取消或超时的响应不恢复请求。
    if (pending.expected !== message.type) throw new TypeError('unexpected signaling response');
    pending.finish(undefined, message.payload);
  }

  #request(type, payload, expected, { signal, timeoutMs = 15000 } = {}) {
    if (this.#closed) return Promise.reject(this.#closed);
    if (signal?.aborted) return Promise.reject(signal.reason ?? new Error('signaling aborted'));
    if (!Number.isFinite(timeoutMs) || timeoutMs <= 0 || timeoutMs > 60000)
      return Promise.reject(new RangeError('invalid signaling deadline'));
    if (this.#pending.size >= 32) return Promise.reject(new RangeError('signaling request limit'));
    const requestID = crypto.randomUUID();
    return new Promise((resolve, reject) => {
      const finish = (error, value) => {
        if (!this.#pending.delete(requestID)) return;
        clearTimeout(timer);
        signal?.removeEventListener('abort', abort);
        if (error !== undefined) reject(error);
        else resolve(value);
      };
      const abort = () => finish(signal.reason ?? new Error('signaling aborted'));
      const timer = setTimeout(() => finish(new Error('signaling request timed out')), timeoutMs);
      this.#pending.set(requestID, { expected, finish });
      signal?.addEventListener('abort', abort, { once: true });
      try {
        const text = encodeControl({
          v: 2,
          type,
          request_id: requestID,
          connection_generation: this.#binding.connectionGeneration,
          session_id: this.#binding.sessionID,
          payload,
        });
        if (this.#socket.bufferedAmount + new TextEncoder().encode(text).length > 128 * 1024)
          throw new RangeError('signaling output limit');
        this.#socket.send(text);
      } catch (error) {
        finish(error);
      }
    });
  }

  async exchangeOffer(description, options) {
    if (description?.type !== 'offer') throw new TypeError('offer required');
    const answer = await this.#request(
      'offer',
      {
        sdp: description.sdp,
        negotiation_generation: this.#binding.negotiationGeneration,
      },
      'answer',
      options,
    );
    return { type: 'answer', sdp: answer.sdp };
  }

  sendCandidate(candidate) {
    if (this.#closed) throw this.#closed;
    const text = encodeControl({
      v: 2,
      type: 'ice_candidate',
      request_id: crypto.randomUUID(),
      connection_generation: this.#binding.connectionGeneration,
      session_id: this.#binding.sessionID,
      payload: { candidate, negotiation_generation: this.#binding.negotiationGeneration },
    });
    if (this.#socket.bufferedAmount + new TextEncoder().encode(text).length > 128 * 1024)
      throw new RangeError('signaling output limit');
    this.#socket.send(text);
  }

  async requestRelay(options) {
    return this.#request(
      'relay_request',
      {
        negotiation_generation: this.#binding.negotiationGeneration,
      },
      'relay_ready',
      options,
    );
  }

  onCandidate(listener) {
    if (this.#closed) throw this.#closed;
    if (typeof listener !== 'function' || this.#listeners.size >= 8)
      throw new TypeError('bounded candidate listener required');
    this.#listeners.add(listener);
    for (const candidate of this.#candidates.splice(0)) listener(candidate);
    return () => this.#listeners.delete(listener);
  }

  onClose(listener) {
    if (typeof listener !== 'function' || this.#closeListeners.size >= 8)
      throw new TypeError('bounded signaling close listener required');
    if (this.#closed) {
      listener(this.#closed);
      return () => {};
    }
    this.#closeListeners.add(listener);
    return () => this.#closeListeners.delete(listener);
  }

  close() {
    this.#stop(new Error('signaling closed by caller'), 1000);
  }

  get lease() {
    return this.#lease;
  }

  // 只返回当前已认证信令代次的浏览器快照；等待不复用上一代凭据。
  waitForICE(session, { signal, timeoutMs = 10000 } = {}) {
    if (this.#closed) return Promise.reject(this.#closed);
    if (signal?.aborted) return Promise.reject(signal.reason);
    const leaseExpiry = Date.parse(session?.lease_expires_at);
    if (
      session?.session_id !== this.#binding.sessionID ||
      session?.connection_generation !== this.#binding.connectionGeneration ||
      session?.negotiation_generation !== this.#binding.negotiationGeneration ||
      typeof session?.policy_revision !== 'string' ||
      !session.policy_revision ||
      !Number.isFinite(leaseExpiry) ||
      leaseExpiry <= Date.now() ||
      !Number.isFinite(timeoutMs) ||
      timeoutMs <= 0 ||
      timeoutMs > 60000 ||
      this.#iceWaiters.size >= 8
    )
      return Promise.reject(new TypeError('bounded original session ICE wait required'));
    const policyRevision = session.policy_revision;
    return new Promise((resolve, reject) => {
      const pending = {
        finish: (error) => {
          if (!this.#iceWaiters.delete(pending)) return;
          clearTimeout(timer);
          signal?.removeEventListener('abort', abort);
          if (error !== undefined) return reject(error);
          const ice = this.#ice;
          const now = Date.now();
          if (
            !ice ||
            ice.policy_revision !== policyRevision ||
            Date.parse(ice.expires_at) <= now ||
            Date.parse(ice.expires_at) > leaseExpiry ||
            ice.ice_servers.some(
              (server) =>
                server.username &&
                (!Number.isFinite(Date.parse(server.credential_expires_at)) ||
                  Date.parse(server.credential_expires_at) <= now),
            )
          )
            return reject(
              new StaleControlError('ICE snapshot differs from original session authority'),
            );
          resolve(ice);
        },
      };
      const abort = () => pending.finish(signal.reason);
      const timer = setTimeout(
        () => pending.finish(new Error('authorized ICE snapshot timed out')),
        timeoutMs,
      );
      this.#iceWaiters.add(pending);
      signal?.addEventListener('abort', abort, { once: true });
      if (this.#ice) pending.finish();
    });
  }

  waitForLease(lease, { signal, timeoutMs = 10000 } = {}) {
    if (this.#closed) return Promise.reject(this.#closed);
    if (signal?.aborted) return Promise.reject(signal.reason);
    if (
      !lease ||
      typeof lease.grant !== 'string' ||
      !lease.grant ||
      typeof lease.lease_version !== 'string' ||
      !/^[1-9][0-9]{0,18}$/.test(lease.lease_version) ||
      !Number.isFinite(Date.parse(lease.lease_expires_at)) ||
      !Number.isFinite(timeoutMs) ||
      timeoutMs <= 0 ||
      timeoutMs > 60000 ||
      this.#leaseWaiters.size >= 8
    )
      return Promise.reject(new TypeError('bounded applied lease wait required'));
    return new Promise((resolve, reject) => {
      const pending = {
        lease: { ...lease },
        finish: (error) => {
          if (!this.#leaseWaiters.delete(pending)) return;
          clearTimeout(timer);
          signal?.removeEventListener('abort', abort);
          if (error !== undefined) reject(error);
          else resolve(this.#lease);
        },
      };
      const abort = () => pending.finish(signal.reason);
      const timer = setTimeout(
        () => pending.finish(new Error('applied lease timed out')),
        timeoutMs,
      );
      this.#leaseWaiters.add(pending);
      signal?.addEventListener('abort', abort, { once: true });
      if (this.#lease && BigInt(this.#lease.lease_version) >= BigInt(lease.lease_version)) {
        const current = this.#lease;
        pending.finish(
          current.grant === lease.grant &&
            current.lease_version === lease.lease_version &&
            Date.parse(current.lease_expires_at) === Date.parse(lease.lease_expires_at)
            ? undefined
            : new StaleControlError('applied lease differs from renewal response'),
        );
      }
    });
  }
}
