# 浏览器传输与业务页面

安装依赖并构建：

```powershell
npm ci
npm run build
npm run build:viewers
npm test
```

`build` 生成独立测试用 `dist/sdk.js` 及网关 embed 的
`../gateway/internal/viewerui/transport.js`；`build:viewers` 生成 WebService
的成熟协议组件。远程 viewer 按需加载同源网关 SDK，LAN 不依赖网关。

真实页面联调需明确指定已授权的 Connector 管理地址：

```powershell
$env:YOODESK_CONNECTOR_ADMIN_URL='http://127.0.0.1:19446/'
node tests/viewer-transport-browser.mjs
```

脚本创建限时邀请和临时会话，不输出密码或邀请令牌。
`YOODESK_VIEWER_FAULT=turn` 阻断直连候选，验证真实双端 relay；
`YOODESK_VIEWER_FAULT=all` 阻断两种 ICE 路径，验证三阶段 WSS 回退。
`YOODESK_VIEWER_PROTOCOLS=vnc` 可限定页面；`YOODESK_VIEWER_HOLD_MS=25000`
额外验证续租后子 iframe 复用及跨 realm Request。RDP 目标协议连接的验收范围
以项目 STATE 为准，页面联调不代表完整 RDP 功能矩阵通过。
