# Gateway 详细设计

语言 Go。传输与 ICE 优先迁移 YoDesk；账号与设备授权优先迁移 UniRemoteLite-Repeater。Gateway 只识别通用能力及会话范围，不实现 SSH/RDP/RustDesk 业务。协议权威见 [05-protocol](05-protocol.md)。

## 1. 模块接口

| 模块 | 接口职责 | 不依赖 |
| --- | --- | --- |
| runtime | Validate/Start/Ready/Stop，按 services 组合装配 | 业务协议 |
| auth | 账号、浏览器登录、PKCE、刷新、JWKS、撤销 | Connector 本机文件 |
| devices | 绑定、设备目录、控制代次、presence | 桌面引擎 |
| control | 单 WS 写入、版本化消息、心跳/快照同步 | 大流 payload |
| sessions | 授权、状态机、租约、协商代次、ICE 策略快照 | WebService 会话实现 |
| proxy | 设备页面映射、HTTP/WS 流、设备数据连接 yamux | 目标私钥/密码 |
| ice | STUN/TURN listener、资源池、短期凭据、健康/配额 | WebService/Connector 源码 |

内部通过小接口组合：DeviceAuthenticator、SessionAuthorizer、DeviceConnectionRegistry、DataPipe、ICEPolicyResolver、CredentialIssuer。避免让整个应用状态对象传入每个模块。

## 2. 服务模式与监听

`--services gateway,stun,turn` 默认全部；允许 gateway、stun、turn、stun+turn 及其他不冲突组合。启动前只校验已启用模块依赖。纯 ICE 模式不加载账号库/签名私钥，不提供登录网页；TURN 本地认证独立可用。

STUN/TURN 共享端口由一个 dispatcher 接收，不重复 bind。TLS 与 HTTPS 证书配置可各自引用，不能依赖先启动 HTTP 才能运行 TURNS。绑定失败应清理已启动资源，进程退出非零，不返回假 ready。外部 ICE 服务不被当成本进程必需 listener。

## 3. 冻结 HTTP 资源

P 为 `/{product_id}`；除 STUN/TURN 标准端口外，全部加 P。

| 方法/路径 | 身份与作用 |
| --- | --- |
| `GET P/login/`、`P/admin/` | 浏览器入口；admin 是平台/企业权限，并非 Connector 本机管理 |
| `POST P/api/v1/connector-logins/` | 创建 PKCE + 设备公钥授权事务，限流，不直接产生设备访问权限 |
| `POST P/api/v1/connector-logins/{id}/exchange/` | poll_secret + verifier，安全领取并验证身份；未完成 202 |
| `POST P/api/v1/token/challenge/` | 刷新设备证明挑战 |
| `POST P/api/v1/token/refresh/`、`.../refresh/ack/` | 刷新事务与持久化后确认，字段行为迁移原契约 |
| `POST P/api/v1/logout/` | 撤销账号/设备接入凭据，不受害于 Cookie/设备 Token 类型混用 |
| `GET P/api/v1/me/`、`P/api/v1/devices/` | 账号信息与游标设备目录 |
| `PATCH/DELETE P/api/v1/devices/{id}/` | 备注/名称、解绑，严格所有权或授权角色 |
| `POST P/api/v1/device-tokens/` | 使用已授权身份获取短期设备接入 Token，不能匿名签发 |
| `GET P/api/v1/jwks.json` | 验证公钥，不包含私钥 |
| `POST P/api/v1/sessions/` | device_id/capability/target_id，创建远程访问上下文 |
| `GET/DELETE P/api/v1/sessions/{id}/` | 观察/主动撤销 |
| `WS P/connector/control/`、`.../data/` | 设备控制与数据分离，Authorization header |
| `WS P/sessions/{id}/signal/`、`.../relay/` | 当前用户获准会话，Origin/一次性连接绑定 |
| `P/devices/{id}/webclient/{cap}/...` | 从权限校验到短期会话再到反代流 |

此表冻结新项目路径，旧源码含 saas 的包名可以先保留为内部迁移事实，但旧 API URL 不暴露。账号接口字段的精确认证语义迁移测试不得丢失；适配层将旧路径改为新资源名。

## 4. 存储与权威

持久实体：用户/管理员/企业及成员、设备与绑定、客户端授权事务、设备证明挑战、刷新事务和 ACK、凭据撤销版本、ICE 节点/池/策略、审计记录。沿用原迁移顺序，新增配置实体使用新 migration，不直接改历史已发布表。

活跃 registry 保存 device_id→控制连接/代次及 data pipe、session 状态/租约/策略版本、浏览器信令绑定。数据连接必须对应当前有效控制代次，不能只验证一次长期 Token 就永久接管。

每次新会话检查权威授权。撤销会持久更新版本、关闭匹配连接并停止续租；多实例控制面以后设计，不能把内存撤销冒充全局强一致。SQLite 不可用不切换到另一个空数据库；账号错误统一脱敏。

## 5. 页面、WS 与会话

POST sessions 请求绑定用户与目标，返回 opaque session_id、状态、允许 transport、signal_url、relay_url、短期 grant 和策略版本。浏览器 profile 不由 Gateway 解码。只读页面入口也要检查设备访问权限，不因静态文件无秘密而绕过设备 ACL。

页面请求显式映射 `/P/devices/{id}/webclient/...` 到后端 `/P/webclient/...`，不剥产品前缀。根导航/静态资源经普通 HTTPS，交互经 SDK；登录及 Set-Cookie 回路保持浏览器原生语义。过滤 hop-by-hop/身份头、明确配置受信上级反代，不根据客户端 Forwarded 判断本机或企业。

设备 WSS 默认一条控制一条数据，外层 yamux 多用户隔离，每流绑定会话。配额覆盖用户、设备、企业和进程总量；关闭/超时清理映射，旧断线清理不能删除新连接。

## 6. 迁移门槛

先以 fake authenticator 和 echo 后端验证端口、流和生命周期；测试替身只用于测试，发行运行不允许匿名降级。迁移账号时带入真实验签、刷新崩溃恢复、解绑撤销和数据库隔离测试。

独立模式启动/停止、两设备与两用户并发、旧连接乱序注销、控制拥塞、TURN 不依赖账号库均在 T04/T08 验收。相关证据按 [验证设计](08-verification.md) 保存。
