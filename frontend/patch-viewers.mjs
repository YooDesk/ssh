// 固定组件的小范围适配；不修改 node_modules。来源、许可和退出条件见 docs/verification。
import { readFile } from 'node:fs/promises';
function replaceOnce(source, before, after) {
  if (!source.includes(before) || source.indexOf(before) !== source.lastIndexOf(before))
    throw new Error('Viewer upstream changed: review the pinned adaptation');
  return source.replace(before, after);
}
export const viewerAdapters = {
  name: 'viewer-adapters',
  setup(build) {
    build.onLoad({ filter: /@xterm[/\\]xterm[/\\]lib[/\\]xterm\.mjs$/ }, async ({ path }) => {
      let source = await readFile(path, 'utf8');
      // Viewport 使用 mainDocument 而非 documentOverride；仅给其创建的 style 加本页 nonce。
      let count = 0;
      source = source.replace(/\b\w+\.mainDocument\.createElement\("style"\)/g, (expression) => {
        count++;
        return `Object.assign(${expression},{nonce:document.documentElement.dataset.styleNonce})`;
      });
      if (count !== 1) throw new Error('xterm Viewport nonce adaptation needs review');
      return { contents: source, loader: 'js' };
    });
    build.onLoad({ filter: /@novnc[/\\]novnc[/\\]core[/\\]rfb\.js$/ }, async ({ path }) => {
      let source = (await readFile(path, 'utf8')).replaceAll('\r\n', '\n');
      // RFB 扩展已经由 noVNC 解帧；保留全部 screen rect，公开只读布局和选择方法。
      const start = source.indexOf('        for (let i = 0; i < numberOfScreens; i += 1) {');
      const end = source.indexOf('\n        /*', start);
      if (start < 0 || end < 0) throw new Error('noVNC screen parser changed');
      source =
        source.slice(0, start) +
        `        const screens = [];
        for (let i = 0; i < numberOfScreens; i += 1) {
            const id = this._sock.rQshift32();
            const x = this._sock.rQshift16(), y = this._sock.rQshift16();
            const width = this._sock.rQshift16(), height = this._sock.rQshift16();
            const flags = this._sock.rQshift32();
            if (i === 0) { this._screenID = id; this._screenFlags = flags; }
            if (width > 0 && height > 0 && x + width <= this._FBU.width && y + height <= this._FBU.height) screens.push({id:String(id), x, y, width, height});
        }
        if (this._FBU.x !== 1 || this._FBU.y === 0) {
            this._screens = screens;
            if (this._selectedScreen && !screens.some(s => s.id === this._selectedScreen.id)) this._selectedScreen = null;
        }
` +
        source.slice(end);
      source = replaceOnce(
        source,
        '        // Normally we only apply the current resize mode after a',
        "        this.dispatchEvent(new CustomEvent('screens', {detail:{screens:this.screens}}));\n\n        // Normally we only apply the current resize mode after a",
      );
      source = replaceOnce(
        source,
        '    get capabilities() { return this._capabilities; }',
        `    get screens() { return (this._screens || []).map(s => ({...s})); }
    selectScreen(id) {
        const screen = id === null ? null : this._screens?.find(s => s.id === String(id));
        if (id !== null && !screen) throw new Error('Unknown VNC display');
        this._selectedScreen = screen;
        this._updateClip(); this._updateScale(); this._saveExpectedClientSize();
    }
    get capabilities() { return this._capabilities; }`,
      );
      source = replaceOnce(
        source,
        '    _updateClip() {',
        `    _updateClip() {
        if (this._selectedScreen) {
            const screen = this._screens.find(s => s.id === this._selectedScreen.id);
            if (screen && screen.x + screen.width <= this._fbWidth && screen.y + screen.height <= this._fbHeight) {
                this._display.clipViewport = true;
                this._display.viewportChangeSize(screen.width, screen.height);
                this._display.viewportChangePos(-this._fbWidth, -this._fbHeight);
                this._display.viewportChangePos(screen.x, screen.y);
                this._setClippingViewport(true); this._fixScrollbars(); return;
            }
            this._selectedScreen = null;
        }`,
      );
      // 调整单个显示器不应发送“单屏布局”命令覆盖远端已有多屏排列。
      source = replaceOnce(
        source,
        '    _requestRemoteResize() {',
        '    _requestRemoteResize() {\n        if (this._selectedScreen || this.screens.length > 1) return;',
      );
      return { contents: source, loader: 'js' };
    });
  },
};
