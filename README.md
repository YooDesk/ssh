# YooDesk ssh

SSH 多轮认证、加密密钥、跳板链、终端与同连接 SFTP。

插件导出 `yd_plugin_v1`，独立拥有 runtime 和业务会话，通过版本化 C ABI 的宿主回调共享访问权威。
webservice 在运行时加载本库，转发 HTTP/WebSocket；本仓库不依赖宿主 Rust crate 或兄弟仓库路径。
源码来源提交见 `SOURCE.json`，公共接口快照保留于 `protocol/`。

## 构建与发布

`.github/workflows/build.yml` 在 push/手动运行时构建 Windows x64/arm64、Linux x64/arm64 和 macOS arm64/x64。
Linux 原生代码及依赖在 Ubuntu 18.04 中编译，发布包验证 glibc <= 2.27。
macOS 动态库为 `.dylib`，只允许链接 Apple 系统库，x64 切片在 arm64 runner 上交叉编译。
只有全部平台成功才发布最新 Release。每个平台产物为 `yoodesk-ssh-<platform>.zip`
及 `.sha256`，包含动态库、`plugin.json`、浏览器 JS/WASM 和相应运行附件/许可证。
webservice 流水线通过公共 Release API 下载这些产物，不需要额外 PAT。

```sh
cargo build --locked --release --lib
cargo test --locked --lib
python ci/package.py ssh --platform linux-x64
```

本地打包设置 `CARGO_BUILD_TARGET` 为相应 Rust target，使产物位于 `target/<target>/release`。
浏览器依赖使用 `npm ci --prefix frontend --ignore-scripts`，构建命令是 `node ci/browser.mjs ssh`。

## 2026-11-08 SFTP 路由修复

SFTP 按字段名提取 `id`、`tid`，兼容动态插件前缀带来的额外 `capability` 参数，
避免上传、下载、目录和传输接口因路径参数数量不匹配返回 500。
真实 HTTP 路由回归覆盖五类接口，验证它们到达鉴权并正确返回 401。
本机单元测试 67 项通过、2 项既有外部 sftp-server 测试忽略；新 DLL 已打包并安装到 Windows。
四路真实浏览器 STUN SSH/SFTP 同时上传、下载各 16 MiB，SHA256 一致，共同保持超过 185 秒。
