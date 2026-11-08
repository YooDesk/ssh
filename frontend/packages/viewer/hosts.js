import { $, capability } from './access.js';
import { sidebarLayout } from './sidebar.js';
import { hostCard } from './host-card.js';
// 与终端相同的主机维护入口；目标ID仍只引用服务端持久连接目录。
export function hostSidebar(profiles) {
  document.body.classList.add('native-hosts');
  const layout = sidebarLayout();
  $('hosts-title').textContent = `${capability.toUpperCase()} 主机`;
  function render() {
    const filter = $('host-filter').value.toLowerCase();
    $('hosts').replaceChildren(
      ...profiles.items
        .filter((t) => `${t.label} ${t.host}`.toLowerCase().includes(filter))
        .map((target) => {
          return hostCard(target, {
            protocol: capability,
            detail: `${target.host}:${target.port}`,
            current: target.target_id === $('target').value,
            connect: () => {
              if (
                target.target_id === $('target').value &&
                $('status').dataset.state === 'active'
              ) {
                $('stage').querySelector('canvas')?.focus();
                return;
              }
              $('target').value = target.target_id;
              $('target').dispatchEvent(new Event('change'));
              render();
              if (innerWidth < 700) layout.collapse();
            },
            edit: profiles.canManage ? () => profiles.open(target) : null,
            remove: profiles.canManage
              ? () =>
                  void profiles.remove(target).catch((error) => {
                    $('message').textContent = error.message;
                  })
              : null,
          });
        }),
    );
  }
  new MutationObserver(render).observe($('target'), { childList: true });
  $('host-filter').oninput = render;
  render();
  return render;
}
