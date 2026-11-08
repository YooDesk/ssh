// yamux 协议的 JavaScript 实现（浏览器端）
//
// 与 Rust `yamux 0.14.1` 二进制兼容。帧格式来自源码：
//   frame/header.rs  HEADER_SIZE = 12
//   [0]      version (0)
//   [1]      tag: 0=Data 1=WindowUpdate 2=Ping 3=GoAway
//   [2..4]   flags (u16 BE): SYN=1 ACK=2 FIN=4 RST=8
//   [4..8]   stream_id (u32 BE)
//   [8..12]  length (u32 BE)
//
// 语义要点（来自 connection.rs）：
//   - 打开流时发 Data 帧 + SYN。首帧 body 不能超过 256 KiB（DEFAULT_CREDIT）
//   - 对端收到 SYN 后必须回一个带 ACK 的 WindowUpdate（credit 0）或 Data
//   - 每个流的初始接收窗口 256 KiB；收够一半就回 WindowUpdate 补充
//   - GoAway 表示连接关闭

const HEADER_SIZE = 12;
const VERSION = 0;

const TAG_DATA = 0;
const TAG_WINDOW_UPDATE = 1;
const TAG_PING = 2;
const TAG_GOAWAY = 3;

const FLAG_SYN = 1;
const FLAG_ACK = 2;
const FLAG_FIN = 4;
const FLAG_RST = 8;

const DEFAULT_CREDIT = 256 * 1024;

// 单连接最多允许的并发流数。**必须与 HOST 侧 `host/src/mux.rs::MAX_STREAMS` 一致。**
//
// 注意区分两种超限：
// - 本文件是 yamux **客户端**，超限时只回一条 RST 拒掉新流（见 `_onData`），
//   连接本身还在；
// - HOST（服务端）超限时 yamux 0.14.1 直接 `Terminate(Frame::internal_error())`，
//   整条连接断掉。所以真正的硬边界在服务端，这里必须给同样的额度。
const MAX_STREAMS = 256;

// 单个 yamux 帧 body 的上限 —— 这是**传输层**约束，不是 yamux 协议约束。
//
// 跑在 WebRTC DataChannel 上时：webrtc-rs 只接受 <= 16384 字节的消息，
// 而一条消息里装的是「12 字节帧头 + body」，所以 body 最多 16372。
// 超了会被对端丢弃，表现为连接活着但数据全丢。
//
// 跑在普通字节流上（如 stdio、TCP）时没有这个限制，用 yamux 协议允许的
// 最大窗口值即可。调用方通过 `new YamuxConnection(send, {maxFrameBody})`
// 指定，默认取协议上限。
const DEFAULT_MAX_FRAME_BODY = DEFAULT_CREDIT;

/** DataChannel 场景下的帧体上限，供 client.js 使用。 */
export const DC_MAX_FRAME_BODY = 16 * 1024 - HEADER_SIZE;

/**
 * 字节流（WSS 等）场景下的帧体上限，供 `pipe-session.js` 的 WSS 回落路径使用。
 *
 * 对端是 Go 的 hashicorp/yamux 时，单帧可以大到整个流窗口（256 KiB），实测
 * `io.Copy` 起手就是 32 KiB/帧。若沿用 DataChannel 的 16372，接收侧的保护
 * 会把**整条连接**判成协议错误关掉（见 `feed` 里的 `_close()`），表现为
 * 「fetch 大资源时所有流一起断」。
 */
export const STREAM_MAX_FRAME_BODY = DEFAULT_CREDIT;

/** 编码一个 yamux 帧头。 */
function encodeHeader(tag, flags, streamId, length) {
  const buf = new Uint8Array(HEADER_SIZE);
  buf[0] = VERSION;
  buf[1] = tag;
  buf[2] = (flags >> 8) & 0xff;
  buf[3] = flags & 0xff;
  buf[4] = (streamId >>> 24) & 0xff;
  buf[5] = (streamId >>> 16) & 0xff;
  buf[6] = (streamId >>> 8) & 0xff;
  buf[7] = streamId & 0xff;
  buf[8] = (length >>> 24) & 0xff;
  buf[9] = (length >>> 16) & 0xff;
  buf[10] = (length >>> 8) & 0xff;
  buf[11] = length & 0xff;
  return buf;
}

function decodeHeader(buf, off) {
  return {
    version: buf[off],
    tag: buf[off + 1],
    flags: (buf[off + 2] << 8) | buf[off + 3],
    streamId:
      ((buf[off + 4] << 24) | (buf[off + 5] << 16) | (buf[off + 6] << 8) | buf[off + 7]) >>> 0,
    length:
      ((buf[off + 8] << 24) | (buf[off + 9] << 16) | (buf[off + 10] << 8) | buf[off + 11]) >>> 0,
  };
}

/** 一条逻辑流：对外暴露 send / ondata / close。 */
export class YamuxStream {
  constructor(conn, id, sendWindow) {
    this.conn = conn;
    this.id = id;
    this.sendWindow = sendWindow;
    // 我们对对端（HOST）的接收承诺，也就是 HOST→浏览器 方向的发送额度。
    //
    // yamux 协议默认每条流 256 KiB。TURN 上一次 credit 往返是秒级，256 KiB 一发满
    // 就要干等——实测 8 MiB 下载固定卡在 229 KiB 处约 4 秒，单流吞吐被压到
    // 0.5~1 MiB/s（`diag-turn.mjs`）。放大到 1 MiB 让正常响应能一口气发完。
    //
    // 差额在流建立后通过一次 WindowUpdate 补足（见 `_grantInitialCredit`）。
    this.recvWindow = DEFAULT_CREDIT;
    this.pendingReceived = 0;
    this.consumedCredit = 0;
    this.sendBuffer = [];
    this.sendBufferLen = 0;
    this.enqueuedBytes = 0;
    this.sentBytes = 0;
    this.writeWaiters = [];
    this.closing = false;
    this.closeNotified = false;
    this.onData = null; // (Uint8Array) => void
    this.onClose = null; // () => void
    this.onDrain = null; // () => void  可写
    this.closed = false;
    this.remoteClosed = false;
    this.writable = true;
  }

  /** 写入数据。返回 false 表示当前不可写（等 onDrain）。 */
  write(data) {
    if (this.closed || this.closing) return false;
    const chunk = data instanceof Uint8Array ? data : new Uint8Array(data);
    if (chunk.length === 0) return this.writable;
    if (this.sendBufferLen + chunk.length > this.conn.maxSendQueue)
      throw new RangeError('yamux send queue limit exceeded');
    this.sendBuffer.push(chunk);
    this.sendBufferLen += chunk.length;
    this.enqueuedBytes += chunk.length;
    this._flush();
    return this.writable;
  }

  writeAsync(data) {
    if (this.closed || this.closing) return Promise.reject(new Error('yamux stream is closed'));
    const chunk = data instanceof Uint8Array ? data : new Uint8Array(data);
    const target = this.enqueuedBytes + chunk.length;
    this.write(chunk);
    if (this.sentBytes >= target) return Promise.resolve();
    return new Promise((resolve, reject) => this.writeWaiters.push({ target, resolve, reject }));
  }

  /** 半关闭写方向。 */
  closeWrite() {
    if (this.closed || this.closing) return;
    this.closing = true;
    this._flush();
  }

  reset() {
    if (this.closed && this.remoteClosed) return;
    this.conn._sendFrame(TAG_DATA, FLAG_RST, this.id, new Uint8Array(0));
    this._terminate();
    this.conn.streams.delete(this.id);
  }

  _flush() {
    while (this.sendBuffer.length > 0) {
      const head = this.sendBuffer[0];

      // ⚠️ 每个 yamux 帧的 body 还必须再受 DataChannel 单消息上限的约束。
      //    SCTP 单条消息超过 16 KiB 时，webrtc-rs 会直接断开通道
      //    （见 src/data_channel/mod.rs: "OnMessage can currently receive
      //    messages up to 16384 bytes"）。一次 200 KiB 的 write 若不切开，
      //    整个连接会被打掉。
      const canSend = Math.min(head.length, this.sendWindow, this.conn.maxFrameBody);
      if (canSend <= 0) {
        this.writable = false;
        return;
      }
      const slice = head.subarray(0, canSend);
      if (!this.conn._sendFrame(TAG_DATA, 0, this.id, slice)) {
        this.writable = false;
        return;
      }
      this.sendWindow -= canSend;
      if (canSend === head.length) {
        this.sendBuffer.shift();
      } else {
        this.sendBuffer[0] = head.subarray(canSend);
      }
      this.sendBufferLen -= canSend;
      this.sentBytes += canSend;
      this.writeWaiters = this.writeWaiters.filter((waiter) => {
        if (waiter.target <= this.sentBytes) {
          waiter.resolve();
          return false;
        }
        return true;
      });
    }
    if (this.closing && !this.closed) {
      if (!this.conn._sendFrame(TAG_DATA, FLAG_FIN, this.id, new Uint8Array(0))) return;
      this.closed = true;
      if (this.remoteClosed) this.conn.streams.delete(this.id);
    }
    if (!this.writable) {
      this.writable = true;
      this.onDrain?.();
    }
  }

  _onFrame(flags, body) {
    if (flags & FLAG_RST) {
      this._terminate();
      return;
    }

    if (body.length > 0) {
      if (body.length > this.recvWindow) {
        this.conn._close();
        return;
      }
      // 消费接收窗口
      this.recvWindow -= body.length;
      this.pendingReceived += body.length;
      this.onData?.(body);
      if (this.conn.autoConsume) this.consume(body.length);
    }

    if (flags & FLAG_FIN) {
      this.remoteClosed = true;
      this._notifyClose();
      if (this.closed) this.conn.streams.delete(this.id);
    }
  }

  _addCredit(n) {
    if (this.sendWindow + n > 0xffffffff) {
      this.conn._close();
      return;
    }
    this.sendWindow += n;
    this._flush();
  }

  // 流式响应由实际消费者补窗口，避免慢下载时无限累积浏览器内存。
  consume(n) {
    if (!Number.isInteger(n) || n < 0 || n > this.pendingReceived)
      throw new RangeError('invalid receive credit');
    this.pendingReceived -= n;
    this.consumedCredit += n;
    if (this.consumedCredit >= DEFAULT_CREDIT / 4) {
      const credit = this.consumedCredit;
      this.consumedCredit = 0;
      this.recvWindow += credit;
      this.conn._sendFrame(TAG_WINDOW_UPDATE, 0, this.id, new Uint8Array(0), credit);
    }
  }

  _terminate() {
    this.closed = true;
    this.sendBuffer = [];
    this.sendBufferLen = 0;
    for (const waiter of this.writeWaiters) waiter.reject(new Error('yamux connection closed'));
    this.writeWaiters = [];
    this._notifyClose();
  }

  _notifyClose() {
    if (this.closeNotified) return;
    this.closeNotified = true;
    this.onClose?.();
  }
}

/**
 * yamux 连接。浏览器是 Client（开奇数流）。
 *
 * `send` 是一个 (Uint8Array) => void 的回调，把字节写到 DataChannel。
 */
export class YamuxConnection {
  /**
   * @param {(bytes: Uint8Array) => void} send  把字节写到下层通道
   * @param {{maxFrameBody?: number}} [opts]    maxFrameBody 是传输层单帧上限
   */
  constructor(send, opts = {}) {
    this._send = send;
    this.streams = new Map();
    this.nextId = 1; // client 用奇数
    this.lastWriteStream = null;
    this.recvBuf = new Uint8Array(0);
    this.closed = false;
    this.maxFrameBody = opts.maxFrameBody ?? DEFAULT_MAX_FRAME_BODY;
    this.autoConsume = opts.autoConsume ?? true;
    this.maxStreams = opts.maxStreams ?? MAX_STREAMS;
    this.maxSendQueue = opts.maxSendQueue ?? 1024 * 1024;
    this.onStream = null; // (YamuxStream) => void   收到对端开的流
    this.onClose = null;
  }

  /** 喂入从 DataChannel 收到的字节。 */
  feed(bytes) {
    if (this.closed) return;
    if (bytes.byteLength > 1024 * 1024 || this.recvBuf.length + bytes.byteLength > 1024 * 1024) {
      this._close();
      return;
    }
    // 拼接缓冲
    const merged = new Uint8Array(this.recvBuf.length + bytes.length);
    merged.set(this.recvBuf, 0);
    merged.set(bytes, this.recvBuf.length);
    this.recvBuf = merged;

    while (!this.closed && this.recvBuf.length >= HEADER_SIZE) {
      const h = decodeHeader(this.recvBuf, 0);
      if (h.version !== VERSION) {
        console.error('[yamux] bad version', h.version);
        this._close();
        return;
      }

      // ⚠️ 只有 Data 帧的 length 是 body 长度。
      //    WindowUpdate 的 length 是 credit，Ping 的 length 是 ping id，
      //    GoAway 的 length 是错误码 —— 三者 body 恒为 0。
      //    如果按 length 去等 body，窗口更新帧会被误判为「未收全」，
      //    连接就此静默卡死。
      const bodyLen = h.tag === TAG_DATA ? h.length : 0;

      if (h.tag === TAG_DATA && h.length > this.maxFrameBody) {
        console.error(
          `[yamux] data frame exceeds channel max: len=${h.length} max=${this.maxFrameBody} stream=${h.streamId}`,
        );
        this._close();
        return;
      }
      if (this.recvBuf.length < HEADER_SIZE + bodyLen) {
        return; // 帧未收全
      }
      const body = this.recvBuf.subarray(HEADER_SIZE, HEADER_SIZE + bodyLen);
      const rest = this.recvBuf.subarray(HEADER_SIZE + bodyLen);
      this.recvBuf = new Uint8Array(rest);

      this._onFrame(h, body);
    }
  }

  _onFrame(h, body) {
    switch (h.tag) {
      case TAG_DATA:
        this._onData(h, body);
        break;
      case TAG_WINDOW_UPDATE:
        this._onWindowUpdate(h);
        break;
      case TAG_PING:
        if (!(h.flags & FLAG_ACK)) {
          // 回 pong
          this._sendFrame(TAG_PING, FLAG_ACK, 0, new Uint8Array(0), h.length);
        }
        break;
      case TAG_GOAWAY:
        console.info('[yamux] goaway');
        this._close();
        break;
      default:
        this._close();
    }
  }

  _onData(h, body) {
    const isSyn = (h.flags & FLAG_SYN) !== 0;

    if (isSyn) {
      // 对端新开的流
      if (this.streams.size >= this.maxStreams || this.streams.has(h.streamId)) {
        console.error('[yamux] too many streams');
        this._sendFrame(TAG_DATA, FLAG_RST, h.streamId, new Uint8Array(0));
        return;
      }
      const s = new YamuxStream(this, h.streamId, DEFAULT_CREDIT);
      this.streams.set(h.streamId, s);
      // 回 ACK
      this._sendFrame(TAG_WINDOW_UPDATE, FLAG_ACK, h.streamId, new Uint8Array(0), 0);
      this.onStream?.(s);
      if (body.length > 0) s._onFrame(0, body);
      if (h.flags & FLAG_FIN) s._onFrame(FLAG_FIN, new Uint8Array(0));
      return;
    }

    if (h.flags & FLAG_ACK) {
      // 对我们开流请求的确认，无需处理
    }

    const s = this.streams.get(h.streamId);
    if (s) {
      if (h.flags & FLAG_RST) {
        s._terminate();
        this.streams.delete(s.id);
        return;
      }
      // 数据和 FIN 可能在同一帧里，两者都要处理。
      //
      // 这里曾经写成 `body.length === 0 && FIN`，导致「最后一帧数据 + FIN」
      // 的组合把 FIN 丢掉，接收方永远等不到 onClose，表现为传输看似正常
      // 但接收方一直挂在那里等。
      if (body.length > 0) s._onFrame(0, body);
      if (h.flags & FLAG_FIN) s._onFrame(FLAG_FIN, new Uint8Array(0));
    } else if (body.length > 0 || h.flags & FLAG_FIN) {
      // 未知流：按协议不视为错误，忽略
    }
  }

  _onWindowUpdate(h) {
    const isSyn = (h.flags & FLAG_SYN) !== 0;
    if (isSyn) {
      if (this.streams.size >= this.maxStreams || this.streams.has(h.streamId)) {
        this._close();
        return;
      }
      // 对端用 WindowUpdate 开流
      const s = new YamuxStream(this, h.streamId, DEFAULT_CREDIT + h.length);
      this.streams.set(h.streamId, s);
      this._sendFrame(TAG_WINDOW_UPDATE, FLAG_ACK, h.streamId, new Uint8Array(0), 0);
      this.onStream?.(s);
      return;
    }
    const s = this.streams.get(h.streamId);
    if (s) {
      if (h.length > 0) s._addCredit(h.length);
      if (h.flags & FLAG_FIN) {
        s.remoteClosed = true;
        s._notifyClose();
        if (s.closed) this.streams.delete(s.id);
      }
    }
  }

  /** 打开一条新流，返回 YamuxStream。 */
  openStream() {
    if (this.closed) throw new Error('yamux connection is closed');
    if (this.streams.size >= this.maxStreams) throw new RangeError('yamux stream quota exceeded');
    // wire流ID只有32位，耗尽后拒绝新流，不能截断并复用旧标识。
    if (this.nextId > 0xffffffff) throw new RangeError('yamux stream ID exhausted');
    const id = this.nextId;
    this.nextId += 2; // 奇数递增
    const s = new YamuxStream(this, id, DEFAULT_CREDIT);
    this.streams.set(id, s);
    let accepted;
    try {
      accepted = this._sendFrame(TAG_DATA, FLAG_SYN, id, new Uint8Array(0));
    } catch (error) {
      this._close();
      throw error;
    }
    if (!accepted) {
      // SYN没有写出时不能返回可用流；释放名额但不复用已分配的流ID。
      s._terminate();
      this.streams.delete(id);
      throw new Error('yamux stream open blocked by transport backpressure');
    }
    return s;
  }

  _sendFrame(tag, flags, streamId, body, lengthOverride) {
    if (this.closed) return false;
    const len = lengthOverride !== undefined ? lengthOverride : body.length;
    const hdr = encodeHeader(tag, flags, streamId, len);
    if (body.length > 0) {
      const out = new Uint8Array(HEADER_SIZE + body.length);
      out.set(hdr, 0);
      out.set(body, HEADER_SIZE);
      const accepted = this._send(out) !== false;
      if (accepted && tag === TAG_DATA) this.lastWriteStream = streamId;
      if (!accepted && (tag !== TAG_DATA || flags !== 0)) this._close();
      return accepted;
    } else {
      const accepted = this._send(hdr) !== false;
      // FIN由流保存closing状态并在可写时重试；RST是单次控制动作，
      // 写不出时退休原连接，不能只清本机流并让对端继续占用资源。
      if (!accepted && (tag !== TAG_DATA || flags & FLAG_RST)) this._close();
      return accepted;
    }
  }

  resumeWrites() {
    if (this.closed) return;
    // 从上次实际发送数据的流之后轮转。固定从首流开始会让持续大流
    // 抢光每次低水位释放的空间，后续心跳/交互流一直无法发送。
    const streams = [...this.streams.values()];
    const start = streams.findIndex((stream) => stream.id === this.lastWriteStream) + 1;
    for (let offset = 0; offset < streams.length; offset++) {
      if (this.closed) return;
      const stream = streams[(start + offset) % streams.length];
      if (!stream.closed) stream._flush();
    }
  }

  _close() {
    if (this.closed) return;
    this.closed = true;
    // 关闭后不能继续处理同批次中残留的SYN，也不保留尚未消费的数据。
    this.recvBuf = new Uint8Array(0);
    for (const s of this.streams.values()) {
      s._terminate();
    }
    this.streams.clear();
    this.onClose?.();
  }
}

export const YAMUX = {
  HEADER_SIZE,
  DEFAULT_CREDIT,
  TAG_DATA,
  TAG_WINDOW_UPDATE,
  TAG_PING,
  TAG_GOAWAY,
  FLAG_SYN,
  FLAG_ACK,
  FLAG_FIN,
  FLAG_RST,
};
