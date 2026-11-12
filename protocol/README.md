# Protocol — 当前 v2 候选与历史 v1

会话授权采用独立的 [grant v3](session-grant-v3.schema.json) 和[限速快照](viewer-rate-v1.schema.json)。
Control、HTTP API 和流头保持 v2；浏览器原样转交不透明 grant，Gateway 与 Connector 验签并拒绝旧 audience。参见[结构向量](viewer-rate-vectors.json)、[行为场景](viewer-rate-scenarios.json)和[设计32](../docs/design/32-connector-viewer-rate.md)。
首次新建冻结签名快照、每Viewer独享；不新增动态限速消息/ACK/心跳。目标100000/1500，正式压测要求1000/150及36矩阵。

当前Edge业务入口：`info.viewer_session_rpc=2` 声明 `viewer_session`，Edge
须先查询能力再发送新操作；旧 Global 返回不支持时明确拒绝，不跨入口重放。
`page`仅接受设备ID/会话码；`read/bind/renew`仅接受原访客会话证明，bind沿用原SID，不再提供clone，不能自报
node/origin 或替换设备。Global 按实际认证的 Edge 入口创建访客父会话，账号权限
与 WebService 密码校验保持独立。旧账号操作不接受访客 Cookie。
RBS1/RBR1 的 `cookie_token` 新增 `guest:` 命名空间；RBR1 可选 `resource_path`
请求单条 HTTP 资源许可，`upgrade=true` 仅随资源路径出现并关联续租。
返回许可的 header/stream 类型必须一致；路径固定为产品 webclient 资源，不能
指定任意目标或后端。新增字段只由已声明能力的 Gateway 两端发送；旧 relay
消息不变，未改历史 v1 或 Rust/浏览器公开传输布局。当前为候选扩展，需两端
同步升级；新 Edge 对旧 Global 明确返回业务不可用。

Edge支持账号Origin HTTPS直达，以及既有19443 mTLS控制连接。auto在业务发送前选择
安全直达或RPC；失败的写请求不跨通道重放，不增加SSH端口。RPC新增有界 `connector_http`
调用 Global 现有账号处理器。`info.connector_account_rpc=1` 声明能力；旧Global未声明时
Edge不发送新操作，避免旧Schema拒绝导致共享控制连接受损。公钥继续从原info响应取得。
桌面授权网页位于 `/{product_id}/connector-account/` 的独立路径与 Cookie 命名空间，
不与 Edge 的普通浏览器登录态混用；登录返回 DTO 不变，仅 authorization_url 指向此入口。
Connector 的 HTTPS 接入 origin 与受信 `YOODESK_ACCOUNT_ISSUER` 分开：前者始终为 Edge，
后者用于 Global JWT 验签和既有 DPoP htu，不改签名校验规则。
废弃 `edge.global_account_url`；配置只保留原 `cluster.controller_url`，无需第二端口。
新增操作仅在Gateway两端使用，Go/Python共享6个正反向量；候选锁仅更新该Schema、向量及设计文档摘要。

`connector-local-settings-v2.schema.json` 与 `vectors-v2/connector-local-settings.json`
定义新增的 Connector/WebService 本机设置适配。它不修改远程控制 wire、
Gateway grant 或原业务认证；旧 WebService 没有该适配时明确显示不可管理，
不能伪造保存成功。设置不返回访问密码，协议总开关保留单项选择。

`gateway-runtime-inventory-v2.openapi.json` 新增 Global 平台管理员的只读 TURN 池与节点状态查询；`vectors-v2/runtime-inventory.json` 区分配置、监听租约与当前代次探测，并禁止内部端点等额外字段。原 HTTP、节点及 Connector 协议不变。

`gateway-account-forms-v2.openapi.json`定义Global与Edge普通邮箱注册、登录、找回及三种提供者扫码表单；Edge经`account_http`节点RPC调用Global，事务与会话绑定受审node/Origin。`edge-local-admin-v2.openapi.json`定义独立Edge本站管理员初始化、登录、设置、改密、退出与本站设备页面。`admin_devices`仅允许cursor/limit，Global按受审节点、当前有效路由和站点授权筛选；本地管理员Cookie不离开Edge，ICE分配不授予设备可见性。这些是候选协议增量，旧节点不支持新RPC时明确失败，需要两端配套更新，不修改历史v1；验证事实见STATE。

分级接入后，Global 将已认证 Edge 的设备管理范围扩展到其批准的子树；在线状态来自当前设备数据连接。`admin_device_remark` 用同一范围检查写入、匹配原备注并审计。两个操作都不接受请求传入节点或站点。逐级 TLS 隧道的协商和路径格式见 [多级 Edge 组网设计](../docs/design/33-edge-hierarchy.md)。

`gateway-console-v2.openapi.json`定义Global独立管理员管理台十二操作；`vectors-v2/console.json`覆盖严格必需输入及版本。设置存储不代表外部登录已可用；当前远程只取运行态与当前持久授权，desired策略不冒充执行ACK。实现与未完成消费者状态见STATE。

`gateway-onboarding-v2.openapi.json`新增产品入口、首次管理员配置、私有初始化码校验及样式六操作。初始化独立于OIDC，原Connector transaction_id确认和管理员独立会话保持；既有管理员不可覆盖。当前交付/真实域名验收见STATE。

新增`gateway-relay-limits-v2.openapi.json`定义平台/企业限额GET/PUT、设备原子批量PUT和平台表单GET/POST八操作。全局默认、原企业/设备覆盖，单调revision CAS、事务审计、当前企业权限与精确Mbps换算；仅增量HTTP，不修改历史v1或Connector/浏览器控制wire。applied仍须独立执行证明，准确状态见STATE与[限额管理说明](../docs/design/02-gateway.md#中继限额管理入口)。

独立`edge-data-socket-v1.schema.json`及9项正反向量约束Edge节点数据连接的begin/admitted，绑定mTLS身份及当前控制boot/connection_generation，admitted增加独立data_generation。Go/Python校验共享Schema及嵌入副本；与设备Native数据协议分开。业务流头/会话许可及路由快照仍需单独装配，物理yamux ready不授予业务开流权限，见[设计17](../docs/design/17-edge-node-data.md)及STATE。

独立device-admission-challenge/proof-v1 Schema及9/10格式向量定义设备持有证明。Go有实际ES256签发/验证，Go/Python共同校验格式；Global持久挑战/站点授权/一次性消费、网络注册与Rust签名互通尚未实现，边界见[密码学验证](../docs/verification/device-proof-2026-10-31.md)。

独立`device-route-v1.schema.json`与14向量约束Global当前路由绑定，Go嵌入副本/Python一致。它不授予设备访问权；目前仅内部持久存储基础，设备证明/站点授权与网络注册尚未接入，证据见[路由存储验证](../docs/verification/route-store-2026-10-31.md)。

当前实施目标：CANDIDATE-v2。新契约采用 `*-v2.schema.json`、`*-v2.openapi.json`、`profiles-v2.json` 与 `vectors-v2/`；`lock.json` 校验当前候选。修订原因、兼容策略与稳定门槛见 [v2 修订](../docs/design/10-revision-v2.md)。传输使用 **yamux**；Axum 是 Rust HTTP 框架，两者不冲突。

新增`account-authority-v2.openapi.json`记录Connector可信确认/OIDC回调四个页面操作。它是Global账号核心的增量入口，不改已有客户端领取/刷新DTO，不等于Edge认证代理或持久浏览器账号会话已实现。运行证据见 [公开权威验证](../docs/verification/gateway-authority-2026-10-31.md)。

`browser-account-v2.openapi.json`记录独立浏览器账号Cookie/目录/退出/刷新ACK与可信脚本共11操作，Cookie名称与已有gateway-access-v2的AccountSession一致；`browser-revoke-v1.schema.json`是用途受限撤销证明（独立新v1，非历史客户端v1），11向量同时经Python Schema与Go真实验签验证。Edge RPC、持久退出队列和产品自动续期客户端已有实现；`edge-logout-record-v1.schema.json`及7向量约束pending证明与ACK后无凭据tombstone。刷新pending期间的RID Cookie仅用于退出双提交，不能用于登录或读取账号；准确验收状态见STATE，历史基础验证见[浏览器账号验证](../docs/verification/browser-account-2026-10-31.md)。

新增节点协议为独立候选v1，不属于历史客户端FROZEN-v1：`edge-control-v1.schema.json`、`ice-node-v1.schema.json`共用`node-control-v1.schema.json`，向量在`vectors-v2/node-control.json`。它们只在Gateway的Global/worker两端使用，不要求浏览器/Connector发送节点消息；现有control-v2保持原样。端点、方向、配置摘要、重连和回滚见 [节点wire](../docs/design/13-node-control-wire.md)。Python/Go共同校验Schema/向量；mTLS网络回归位于`gateway/internal/nodecontrol/link_test.go`。配置ACK不等于路由/外部ICE探测就绪。

`edge-auth-rpc-v1.schema.json`在Edge控制连接增加白名单认证、取消及有界响应分片，ICE连接禁止；启用auth的Edge配置增加固定origin，作为ConfigDigest最后一个可选字段，未启用/纯ICE时省略，原五字段摘要不变。规范见[14](../docs/design/14-edge-auth-rpc.md)，Node向量扩为27项；旧节点不理解新字段时明确拒绝，不匿名降级。认证载荷不可记录。

Native设备控制前置准入新增`device-admission-socket-v1.schema.json`及8向量，使用独立`raccess.device-control.v1`子协议。完成同一物理socket的持有证明/持久一次挂接后继续control-v2格式消息；旧raccess.control.v2的首包与认证约定不变。设备RPC增加attach/renew/close，binding须核对实际mTLS来源；节点向量44项。兼容、截止和挂接水位见[Native准入](../docs/design/15-native-device-admission.md)，完成度见STATE，不据注册结果推断数据通路就绪。

Native数据连接新增独立device-data-challenge/proof/socket-v1契约及12/11/8共享向量，子协议raccess.device-data.v1。证明绑定原控制route_epoch及单调data_generation，完成实际yamux Ping及Global ready才可供开流；独立30秒租约/10秒续租。设备RPC增加data_begin/commit/ready/renew/close，Node向量54项。见[数据契约16](../docs/design/16-native-device-data.md)，不改变旧raccess.data.v2及RAX2布局，不替代跨Edge节点隧道。

下表为历史 FROZEN-v1 资产，保持原字节；原始锁和对应设计文档保存在 `history/v1/`，标签 protocol-v1 不移动。v1 不再作为新实现目标。原生 terminal.v1 与 RustDesk protobuf 快照单独版本化，可被外层 v2 继续引用。

| 资产 | 用途 |
| --- | --- |
| `control-v1.schema.json` | Gateway/Connector/浏览器控制消息及网络配置 |
| `business-v1.schema.json` | WebService 会话创建、描述、事件与终端控制 |
| `webservice.openapi.json` | WebService 冻结的 HTTP/WS 路由模板与资源 DTO |
| `session-grant-v1.schema.json` | 解码并验签后的远程会话授权 claims |
| `stream-header-v1.schema.json` | yamux 子流的绑定、HTTP 和 relay 应用头 |
| `backend-context-v1.schema.json` | 可选受信反代对业务动作的限制，不是 Gateway JWT |
| `profiles-v1.json` | 各业务能力数据 profile、限制及路由 |
| `vectors/` | 合成正/反 JSON 示例、应用流头二进制向量 |
| `rustdesk/message-v1.proto` | 只作为端侧消息协议快照，不含 RustDesk UI/服务端 |
| `sources.json` | 来源提交、用户参考工作树与原始摘要 |
| `history/v1/lock.json` | 历史 v1 冻结资产与文档 SHA256 |

详细传输契约见 [05-protocol](../docs/design/05-protocol.md)，业务接口见 [04-webservice](../docs/design/04-webservice.md)。Schema 的附加业务条件、身份校验与 native profile 语义必须同时实施，不能只通过 Schema 就认为授权安全。

运行 `python scripts/check_docs.py` 可验证锁和示例。该工具不会自动重写锁。正向示例的 Token/ID 全部是占位测试数据，不表示有效签名或真实授权。

`connector-invitation-v2.schema.json` 与 `vectors-v2/connector-invitation.json`
定义受保护回环邀请签发：默认一小时的 URL 令牌和八位临时密码共享授权范围与
截止时间。链接使用 `#invite=`，由浏览器清除片段后提交现有 HTTPS 登录 API，
不改变长期访问密码或把秘密写入 HTTP 查询日志。记录只持久保存摘要；关协议、
关总开关或改密会提前撤销。Node DTO 回归：
`node --test frontend/tests/connector-local-settings.test.mjs`。

冻结资产 Git 属性设为保留原字节，避免跨平台 CRLF 转换使快照摘要漂移。第三方 proto 的许可证保存在 rustdesk/LICENSE.AGPL-3.0，不能将其当成本项目原创或不同许可。


Android/account-device migration from upstream c0975f2 adds authenticated Connector
PATCH/DELETE account-device operations and the current node-control candidate's
`device_revoke` message, bound to device ID, route epoch and connector generation.
The optional `info.device_revoke` capability gates revocation notifications to upgraded
Edge peers; old peers retain their existing lease and authority checks. Node schemas
and positive/negative vectors are synchronized across repository snapshots. Historical
client v1 and history/v1 remain unchanged; the native plugin ABI remains version 1.
