import { $ } from './access.js';
const query = new URLSearchParams(window.__yoodeskViewer?.query ?? location.search);
export const embedded = query.get('viewer') === '1' && parent !== window;
export const requestedTarget = query.get('target');
if (embedded) {
  document.body.classList.add('embedded-viewer');
  const publish = () =>
    parent.postMessage(
      {
        type: 'yoodesk.viewer-state',
        status: $('status').textContent,
        state: $('status').dataset.state,
        message: $('message').textContent,
      },
      location.origin,
    );
  for (const id of ['status', 'message'])
    new MutationObserver(publish).observe($(id), {
      childList: true,
      characterData: true,
      subtree: true,
      attributes: true,
    });
  document.addEventListener('access-expired', () =>
    parent.postMessage({ type: 'yoodesk.viewer-access-expired' }, location.origin),
  );
  publish();
}
