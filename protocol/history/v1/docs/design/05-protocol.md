# 冻结协议：Transport/Control v1

状态：**FROZEN-v1（接口冻结，尚未实现）**。本文件与 `protocol/` 下的 Schema、快照、向量、锁文件共同构成协议基线。不得用实现困难为理由静默改字段、增加第二套协议或回退到旧 URL。业务数据协议见 [WebService](04-webservice.md)。

## 1. 范围和版本

- 所有应用路径前缀为 `P=/{product_id}`，完整示例 `/yoodesk/connector/control/`。P 不出现在稳定协议标识、二进制 magic 或消息类型中。
- 控制 WS 子协议为 `raccess.control.v1`，数据 WS 为 `raccess.data.v1`，浏览器信令为 `raccess.signal.v1`，回退流为 `raccess.relay.v1`。握手不支持指定版本时 HTTP 426；已升级后协议违反使用 WS 1002。
- JSON 使用 UTF-8，ID/版本/代次统一为字符串，避免 JS 大整数精度问题。`request_id` 为当前连接内唯一不透明字符串；`session_id` 指 Gateway 的远程会话。
- 控制包格式固定为 `{v,type,request_id,connection_generation,session_id?,payload}`，`v=1`。字段类型/必填项由 `protocol/control-v1.schema.json` 约束。
- JSON 控制消息上限 64 KiB，SDP 单字段上限 48 KiB，candidate 单条上限 4 KiB。超限关闭 1009；心跳不装载设备全目录。
- 未知必需特性或消息返回 `unsupported_message`；未知可选字段可以忽略，不能忽略身份、版本和授权字段校验。破坏性字段/语义变更增加主版本；新能力通过显式协商启用。

## 2. WSS 控制生命周期

Connector 先通过 HTTPS 授权接口取得短期设备接入 Token，在 `P/connector/control/` 的 Authorization header 发送，不放 query。Gateway 从 Token 和权威状态确定设备、企业与会话，不信任自行申报的身份。

Gateway 在认证成功后分配不可复用的 `connection_generation`，首包为 `registered`；Connector 在取得代次前不能发送业务控制消息。同设备新连接接管时旧代次失效，旧连接的心跳、注销和异步结果不能改变新连接状态。

| 消息 | 方向 | payload 必需内容 | 结果 |
| --- | --- | --- | --- |
| `registered` | G→C | `device_id,heartbeat_interval_seconds,device_lease_seconds`；可含网络快照 | 建立设备控制上下文 |
| `heartbeat` | C→G | `applied_network_revision,network_config_status,capabilities_revision` | 检查身份/租约及配置版本 |
| `heartbeat_ack` | G→C | `lease_expires_at`；可含 `network_config` | 续设备租约，按需给新配置 |
| `capabilities` | C→G | `revision,items[]` | 每项 `id,supported,enabled,ready,protocols[]` |
| `capabilities_ack` | G→C | `revision` | 确认已登记快照 |
| `network_config_changed` | G→C | `revision` | C 立即心跳取最新快照 |
| `session_open` | G→C | `grant,ice_config,capability,target_id` | C 验权后准备会话 |
| `session_accept` | C→G | `features[]` | 允许 SDP 协商或 WSS 回退 |
| `session_reject` | C→G | `code,message` | 会话失败，不开数据资源 |
| `offer/answer` | 双向，经 G | `sdp,negotiation_generation` | 按 session/代次转发 |
| `ice_candidate` | 双向，经 G | `candidate,negotiation_generation` | candidate 为对象或 null，null 表示采集结束 |
| `relay_request/relay_ready` | C或浏览器→G / G→双方 | `negotiation_generation` | 按策略分配 WSS，不含未授权房间 |
| `lease_renew/lease_ack` | G→C / C→G | `lease_expires_at` / `accepted` | 远程会话续租 |
| `ice_config_update` | G→C及浏览器 | `ice_config,negotiation_generation` | 刷新凭据/候选后执行显式协商 |
| `session_close/session_closed` | G→C / C→G | `reason` | 关闭该远程会话资源 |
| `error` | 双向 | `code,message,retryable` | 引用原 request_id，错误正文不含秘密 |

除设备注册/心跳/能力类消息外，所有会话类消息必须带 session_id。会话协商代次在 payload 中独立于设备连接代次，切换连接或传输时验证两者。

浏览器通过 Gateway Cookie + CSRF 创建远程会话，用同源登录 Cookie 建立信令 WSS，校验 Origin。Connector 控制身份、浏览器登录态、远程授权票据不得混用。

## 3. 授权票据

Gateway 签名 JWT 的必填字段：`iss,aud,sub,iat,nbf,exp,jti,device_id,session_id,connection_generation,negotiation_generation,capability,target_id,scope,policy_revision`。`aud=raccess-session-v1`；`scope` 为显式操作集合；`target_id` 是后端配置引用而非任意地址。JWT 算法固定为迁移后选定的 RS256 白名单，不接受 `none` 或由消息自行指定的信任根。

Connector 使用固定 Gateway origin 的 JWKS，验证签名、用途、所有范围及当前租约；收到 session_open 不代表可以省略数据面绑定。浏览器发出的 DataChannel 首条会话控制流必须提交该 grant；Connector 与已获授权的 SDP fingerprint/代次关联，5 秒未完成绑定则关闭。票据不能授予本机 admin 或任选 TCP 目标。

票据签发需同时检查设备 ready、权限与资源策略；有效期不超过授权租约。新 grant 由明确续租/协商消息发放，旧 jti/代次不能开启新流。关闭状态为终态，不因晚到消息恢复。

## 4. 数据面：冻结为 yamux

不实现第二套 `OPEN/DATA/WINDOW_UPDATE` 私有流控。使用标准 yamux 的帧头、窗口、半关闭和 RST；复用 YoDesk 的 Go/Rust/JS 互通实现及回归。WS 二进制消息是有序字节块，yamux 帧可以跨消息边界，禁止依赖一次 recv 对应一帧。

```text
Gateway ⇄ Connector：一条设备数据 WSS → 外层 yamux
  ├─ 普通 HTTP/升级 WS：一个 outer stream
  └─ 浏览器 WSS 回退：一个 outer stream → 会话内层 yamux

浏览器 ⇄ Connector：WebRTC DataChannel → 会话内层 yamux
```

Gateway 为外层 yamux client（开奇数流），Connector 为 server；会话内浏览器是 client，Connector 是 server。控制面不在这些数据窗口内。大流隔离以后可增加少量设备数据连接，不改变会话语义。

每条新 yamux stream 的应用头：4 字节 ASCII `RAX1` + 4 字节无符号大端 JSON 长度 N + N 字节 UTF-8 JSON，`1≤N≤16384`。JSON 不允许重复键。头后直接传对应字节流，不加私有 DATA 帧。

| `kind` | 使用链路 | 头字段及后续内容 |
| --- | --- | --- |
| `bind` | 会话内首条控制流 | `grant`；C 回应用头 `{kind:"bind_result",ok:true}` 后，该流保留为 NDJSON 控制/关闭通道 |
| `http` | 外层及已绑定内层 | `session_id,negotiation_generation,capability,target_id,path`；后续一个 HTTP/1.1 请求及响应 |
| `relay` | 外层 | `session_id,negotiation_generation`；后续为浏览器—C 内层 yamux 字节 |

`http` 的原始请求路径必须与头中的 path 规范化后一致；头只作预检，不能靠它掩盖实际请求访问其他路由。拒绝 CONNECT、绝对 URL、不明确的 Content-Length/Transfer-Encoding、跨能力路径和 admin/internal 路径。每流一个 HTTP 交换；101 后按 WS 帧双向复制，保留消息边界、FIN 和关闭码。使用已有 HTTP/WS 库或经过回归的解析适配，不自写宽松解析器。

普通 HTTP 外层流也必须绑定已批准的远程 session。首次页面访问由 Gateway 完成权限检查并创建短期远程访问上下文，然后开流，不能用任意设备 ID 直接代理。Gateway 登录 Cookie 不传给 WebService；WebService 自己的业务 Cookie 按设备路由作用域处理，不用 Gateway 身份伪造本地登录。

Connector 剥离请求自带的 `X-RAccess-*`，对支持受信反代适配的后端重新生成 X-RAccess-Context；字段及细粒度权限语义见 04-webservice §1.1。该上下文由本地调用方认证保护，不转发 Gateway JWT，不把 Connector 变成业务协议解析器。

WSS relay 的浏览器连接使用获授权 session 的 Cookie/短期一次性连接授权，授权放首条有超时限制的绑定消息，不放 URL。在绑定完成前限制缓冲，不能开始转发。Gateway 将连接绑定到当前外层 stream，房间名不能独立充当授权。

## 5. 限制与失败语义

初始限制：内层 256 流，外层每设备 256 流；实际同时会话数由 Gateway 配额另行限制。初始 DataChannel 消息上限 16384 字节，yamux body 至多 16372（含 12 字节帧头）；这是 v1 配置上限，不是 WebRTC 标准的永久限制。按协商 maxMessageSize 取更小值。

SCTP 发送积压初始 64 KiB，浏览器缓冲发送器必须有低水位恢复；流接收窗口必须满足所用 yamux 版本约束。进程总内存另设硬上限，达到预算拒绝新流，而非不断扩大窗口。轮转调度、独立写任务，慢流不能阻塞读取控制信令。

错误码集合：`unauthorized,forbidden,version_mismatch,unsupported_message,stale_generation,device_offline,capability_not_ready,policy_denied,config_expired,quota_exceeded,backend_unavailable,protocol_error,timeout,session_closed`。HTTP 分别采用 401/403/426/409/503/429/502/504 等明确状态，详见业务错误表。WS 1000 正常、1002 协议错误、1008 策略拒绝、1009 超限、1011 内部失败；1006 只由客户端观察，服务端不得发送。

设备 WSS 断开关闭外层全部流；内层/PC 受远程租约约束。回退/重连创建新 negotiation_generation，不重放请求与输入。PUT/上传是否能续传由业务 transfer API 决定，传输层不猜测幂等性。

## 6. 心跳及配置契约

设备心跳每 15 秒、租约 45 秒；远程会话租约 60 秒，每 20 秒续租。配置时间使用 RFC3339 UTC，执行时转换为单调时钟剩余期限，避免本机时钟倒退延长授权。

`network_config={revision,valid_until,stun_servers,ice_policy_revision}`；省略代表不更新，空 stun_servers 明确清除。applied_network_revision 是**成功应用**的版本；错误状态为 failed 并可附 error_code。Gateway 对未确认版本重发完整快照，重连首包下发当前快照，不依赖消息恰好投递一次。

设备快照不包含会话 TURN 密码、全局节点目录或共享密钥；具体会话 ice_config 完整覆盖设备默认值，不能合并未授权服务器。完整规则见 [ICE](06-ice.md)。

## 7. 冻结与实现门槛

协议 ABI 冻结为 v1，关键消息字段在 Schema 中固化。业务能力未实现则明确报告 unsupported，不返回假成功。协议实现必须通过 Go↔Rust、JS↔Rust 的向量与流测试，再接具体业务。

变更现有字段含义、magic、路由、鉴权或标准协议 profile 属于破坏变更；必须新增 v2 或明确经过用户授权更新基线并同步所有端。正常修复、同语义内部重构和新增已协商的可选能力无需重新请求架构许可。
