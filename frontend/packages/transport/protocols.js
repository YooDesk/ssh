const protocols = (kind, generation) => {
  if (typeof generation !== 'string' || !/^[1-9][0-9]{0,19}$/.test(generation))
    throw new TypeError('explicit negotiation generation required');
  return [`raccess.${kind}.v2`, `raccess.negotiation.${generation}`];
};

export const signalSubprotocols = (generation) => protocols('signal', generation);
export const relaySubprotocols = (generation) => protocols('relay', generation);
