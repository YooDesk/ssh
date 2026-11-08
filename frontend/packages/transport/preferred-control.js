import { connectPreferred } from './preferred.js';
import { SignalControl } from './signaling.js';
import { disposeAfterFailure } from './release.js';

// prepare 返回权威新 grant/ICE/context；openSignal 完成浏览器身份认证并返回已打开的 socket。
// 每阶段真实使用 connectRTC/connectRelay，业务请求不会被重放。
export async function connectPreferredControl({
  context,
  policy,
  prepare,
  openSignal,
  timeoutMs,
  onControl,
  requestedPath,
  signal,
}) {
  if (typeof prepare !== 'function' || typeof openSignal !== 'function')
    throw new TypeError('authorized preparation and signaling connection required');
  const controls = new Set();
  let selectedControl;
  let selectedSession;
  const closeOlder = (current) => {
    for (const control of controls) {
      if (control !== current) {
        control.close();
        controls.delete(control);
      }
    }
  };
  const closeAll = () => {
    for (const control of controls) control.close();
    controls.clear();
  };
  try {
    const link = await connectPreferred({
      context,
      policy,
      timeoutMs,
      requestedPath,
      signal,
      async prepare(path, budget) {
        const approved = await prepare(path, budget);
        let socket;
        try {
          socket = await openSignal(approved, budget);
        } catch (error) {
          await disposeAfterFailure(approved, error, { retrying: false });
          throw error;
        }
        let control;
        try {
          control = new SignalControl(socket, {
            connectionGeneration: approved.connectionGeneration,
            sessionID: approved.context.sessionID,
            negotiationGeneration: approved.context.generation,
          });
        } catch (error) {
          socket.close();
          await disposeAfterFailure(approved, error, { retrying: false });
          throw error;
        }
        controls.add(control);
        selectedControl = control;
        selectedSession = approved.session;
        const signaling = {
          onClose: (listener) => control.onClose(listener),
          onCandidate: (listener) => control.onCandidate(listener),
          sendCandidate: (candidate) => control.sendCandidate(candidate),
          async exchangeOffer(offer) {
            const answer = await control.exchangeOffer(offer, {
              timeoutMs: budget.remaining(),
              signal: budget.signal,
            });
            // 新协商已经收到 Connector 响应，旧信令可安全释放。
            closeOlder(control);
            return answer;
          },
        };
        let configuration = approved.configuration;
        try {
          // 选路等待也消耗授权租约；调用方可从此刻维护原信令，并在下一代前交接。
          await onControl?.(control, approved);
          if (path === 'turn' || path === 'stun') {
            const ice = await control.waitForICE(approved.session, {
              timeoutMs: budget.remaining(),
              signal: budget.signal,
            });
            if (
              path === 'turn' &&
              (ice.ice_transport_policy !== 'relay' ||
                !ice.ice_servers.some((server) =>
                  server.urls.some((url) => url.startsWith('turn:') || url.startsWith('turns:')),
                ))
            )
              throw new TypeError('authorized TURN snapshot requires relay servers');
            if (
              path === 'stun' &&
              (ice.ice_transport_policy !== 'all' ||
                ice.ice_servers.some(
                  (server) =>
                    server.username ||
                    server.credential ||
                    server.urls.some(
                      (url) => !url.startsWith('stun:') && !url.startsWith('stuns:'),
                    ),
                ))
            )
              throw new TypeError('authorized direct snapshot must contain STUN only');
            configuration = {
              iceTransportPolicy: ice.ice_transport_policy,
              iceServers: ice.ice_servers.map(({ urls, username, credential }) => ({
                urls: [...urls],
                ...(username ? { username, credential } : {}),
              })),
            };
          }
          if (path === 'websocket') {
            await control.requestRelay({ timeoutMs: budget.remaining(), signal: budget.signal });
            closeOlder(control);
          }
        } catch (error) {
          control.close();
          controls.delete(control);
          await disposeAfterFailure(approved, error, { retrying: false });
          throw error;
        }
        return {
          ...approved,
          configuration,
          signaling,
          async dispose({ retrying = false } = {}) {
            // 失败后继续选路时，等待 Coordinator 交接新代次再释放旧信令。
            if (!retrying) closeAll();
            await approved.dispose?.({ retrying });
          },
        };
      },
    });
    // 最终实例供普通续租等待lease_update；授权快照不随续租偷偷改变。
    Object.defineProperties(link, {
      signalControl: { value: selectedControl },
      session: { value: selectedSession ? Object.freeze({ ...selectedSession }) : undefined },
    });
    return link;
  } catch (error) {
    closeAll();
    throw error;
  }
}
