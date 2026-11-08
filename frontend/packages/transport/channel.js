import { YamuxConnection, DC_MAX_FRAME_BODY } from './yamux.js';
import { encodeStreamHeader } from './http.js';

// RTCDataChannel 的低水位事件驱动恢复发送；不使用定时器掩盖丢失唤醒。
export function dataChannelMux(channel, closeTransport) {
  if (channel.readyState !== 'open') throw new TypeError('DataChannel is not open');
  channel.binaryType = 'arraybuffer';
  channel.bufferedAmountLowThreshold = 32768;
  const mux = new YamuxConnection(
    (frame) => {
      if (channel.readyState !== 'open') return false;
      const control = frame.length === 12 || frame[1] !== 0;
      const limit = 65536 + (control ? 16384 : 0);
      if (channel.bufferedAmount + frame.length > limit) return false;
      try {
        channel.send(frame);
        return true;
      } catch (error) {
        mux._close();
        throw error;
      }
    },
    { maxFrameBody: DC_MAX_FRAME_BODY, autoConsume: false, maxStreams: 256, maxSendQueue: 262144 },
  );
  const receive = (event) => {
    if (!(event.data instanceof ArrayBuffer) || event.data.byteLength > 16384) {
      mux._close();
      return;
    }
    mux.feed(new Uint8Array(event.data));
  };
  const writable = () => mux.resumeWrites();
  const closed = () => mux._close();
  channel.addEventListener('message', receive);
  channel.addEventListener('bufferedamountlow', writable);
  channel.addEventListener('close', closed);
  channel.addEventListener('error', closed);
  mux.onClose = () => {
    channel.removeEventListener('message', receive);
    channel.removeEventListener('bufferedamountlow', writable);
    channel.removeEventListener('close', closed);
    channel.removeEventListener('error', closed);
    channel.close();
    closeTransport();
  };
  return mux;
}

// RAX2 bind 控制流在整个会话中保持打开，关闭即撤销该会话。
export async function bindMux(mux, grant) {
  if (typeof grant !== 'string' || !grant.length) throw new TypeError('session grant required');
  const control = mux.openStream();
  const ready = new Promise((resolve, reject) => {
    let buffer = new Uint8Array(0);
    let bound = false;
    control.onClose = () => {
      reject(new Error('session binding closed'));
      if (bound) mux._close();
    };
    control.onData = (bytes) => {
      try {
        if (bound) throw new TypeError('unsupported session control message');
        if (buffer.length + bytes.length > 16392) throw new RangeError('bind response too large');
        const combined = new Uint8Array(buffer.length + bytes.length);
        combined.set(buffer);
        combined.set(bytes, buffer.length);
        buffer = combined;
        control.consume(bytes.length);
        if (buffer.length < 8) return;
        if (new TextDecoder().decode(buffer.subarray(0, 4)) !== 'RAX2')
          throw new TypeError('invalid bind response version');
        const length = new DataView(buffer.buffer, buffer.byteOffset).getUint32(4);
        if (length < 1 || length > 16384) throw new RangeError('invalid bind response size');
        if (buffer.length < 8 + length) return;
        if (buffer.length !== 8 + length) throw new TypeError('trailing bind response bytes');
        const result = JSON.parse(
          new TextDecoder('utf-8', { fatal: true }).decode(buffer.subarray(8)),
        );
        if (
          result.kind !== 'bind_result' ||
          result.ok !== true ||
          !/^[1-9][0-9]{0,18}$/.test(result.lease_version)
        )
          throw new Error('session binding denied');
        bound = true;
        buffer = new Uint8Array(0);
        resolve(result.lease_version);
      } catch (error) {
        reject(error);
        mux._close();
      }
    };
  });
  try {
    await control.writeAsync(encodeStreamHeader({ kind: 'bind', grant }));
    return await ready;
  } catch (error) {
    mux._close();
    throw error;
  }
}
