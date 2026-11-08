# 浏览器传输来源

`packages/transport/yamux.js` 和初始 yamux 回归来自本地 YoDesk 的 `web/yamux.js` / `web/test/yamux.test.mjs`，提交 `a41589d052887a15d209b83b7808ab784c72bf2b`。原文件摘要见 `packages/transport/source.json`。参考源文件没有许可证头，参考仓库根目录未找到 LICENSE 文件；保留该事实与本地来源，不虚构第三方授权。

当前适配增加了有界发送/流数、按实际消费补 credit、未知协议终止及作用域化 SDK 接口；原始分片、背压、半关闭用例继续保留。参考工作树只读，没有改写其源码。

HTTP 使用 http-parser-js，WS 帧使用 websocket-driver，SHA-1 握手摘要使用 @noble/hashes，Node 流/Buffer 的浏览器适配及全部依赖版本由 package-lock.json 固定。各 npm 包保留自身许可证；未整体复制参考项目 UI 或引入 RustDesk 服务端。
