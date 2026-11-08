import { visit } from 'jsonc-parser';
import validate from './control-schema.js';

export const MAX_CONTROL_MESSAGE = 64 * 1024;
const encoder = new TextEncoder();
export class StaleControlError extends Error {}

export function decodeControl(text) {
  if (
    typeof text !== 'string' ||
    text.length === 0 ||
    text.length > MAX_CONTROL_MESSAGE ||
    encoder.encode(text).length > MAX_CONTROL_MESSAGE
  )
    throw new TypeError('control message size limit');
  const objects = [];
  let depth = 0;
  let malformed = false;
  const begin = () => {
    if (++depth > 32) throw new TypeError('control JSON nesting limit');
  };
  visit(
    text,
    {
      onObjectBegin() {
        begin();
        objects.push(new Set());
      },
      onObjectProperty(key) {
        const current = objects.at(-1);
        if (current.has(key)) throw new TypeError('duplicate control JSON member');
        current.add(key);
      },
      onObjectEnd() {
        objects.pop();
        depth--;
      },
      onArrayBegin: begin,
      onArrayEnd() {
        depth--;
      },
      onError() {
        malformed = true;
      },
    },
    { disallowComments: true, allowTrailingComma: false, allowEmptyContent: false },
  );
  if (malformed) throw new TypeError('invalid control JSON');
  const message = JSON.parse(text);
  if (!validate(message)) throw new TypeError('control message violates v2 schema');
  return message;
}
export function encodeControl(message) {
  const text = JSON.stringify(message);
  decodeControl(text);
  return text;
}

const directions = {
  connector: new Set([
    'heartbeat',
    'capabilities',
    'session_accept',
    'session_reject',
    'answer',
    'ice_candidate',
    'lease_ack',
    'session_closed',
    'error',
  ]),
  browser: new Set(['offer', 'ice_candidate', 'relay_request', 'session_close', 'error']),
  gateway: new Set([
    'registered',
    'heartbeat_ack',
    'capabilities_ack',
    'network_config_changed',
    'session_open',
    'offer',
    'answer',
    'ice_candidate',
    'relay_ready',
    'lease_renew',
    'ice_config_update',
    'session_close',
    'lease_update',
    'error',
  ]),
};
export function checkControlDirection(message, sender) {
  if (!directions[sender]?.has(message.type))
    throw new TypeError('control sender direction denied');
}
export function checkControlBinding(
  message,
  { connectionGeneration, sessionID, negotiationGeneration },
) {
  if (
    message.connection_generation !== connectionGeneration ||
    message.session_id !== sessionID ||
    (negotiationGeneration !== undefined &&
      message.payload.negotiation_generation !== negotiationGeneration)
  )
    throw new StaleControlError('stale control identity or generation');
}
