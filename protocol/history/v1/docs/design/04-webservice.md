# 冻结协议：WebService Business v1

状态：**FROZEN-v1（外部接口冻结，内部适配待实现）**。WebService 使用 Rust/Tokio/Axum，独立提供本机 HTTP 和局域网 HTTPS。远程调用只是经 Connector 代理同一业务接口。本文不要求 WebService 理解 Gateway 的用户、JWT、VIP、WebRTC 或 TURN。

## 1. 作用域与统一资源

定义 `B=/{product_id}/webclient/{capability}`，capability 为 desk/ssh/sftp/vnc/rdp/shell。`B/` 是页面，`B/api/v1/` 是业务资源。所有 URL 必须完整加产品前缀。

`business_session_id` 是 WebService UUID，与 Gateway 的 remote session_id 不同；不得混用。业务主体 principal 由 WebService 本地访问策略产生，资源与 principal 绑定，不能只凭随机 ID 越权。Connector 调用方认证只证明代理身份，不替代业务/目标协议认证。

| 接口 | 返回/语义 |
| --- | --- |
| `GET B/api/v1/capabilities/` | supported_features、limits、stream_profile、版本；只报告实际实现 |
| `POST B/api/v1/sessions/` | 201，创建会话，正文见 Schema |
| `GET B/api/v1/sessions/{id}/` | 200，状态、功能、目标摘要、当前 challenge；无秘密 |
| `DELETE B/api/v1/sessions/{id}/` | 204，重复关闭同一已授权资源幂等 |
| `WS B/api/v1/sessions/{id}/events/` | JSON 事件与状态，不混杂协议二进制 |
| `POST B/api/v1/sessions/{id}/challenges/{cid}/responses/` | 202，提交认证/信任确认/会话选择，不自动重试 |
| `WS B/api/v1/sessions/{id}/stream/` | 使用该能力固定 profile；未 ready 返回 409 |

创建请求字段：`protocol_version=1,target_id,credential_ref?,options,client_features[]`。target_id 引用服务端允许目标，shell/desk 固定为 local；禁止浏览器传任意文件路径或 TCP destination 进行代理。临时凭据通过一次性资源导入，创建请求仅引用 credential_ref。

响应为 `{v:1,data:{business_session_id,state,stream_profile,features,event_url,stream_url},request_id}`；相对路径从 B 生成。状态集合：created、connecting、waiting_challenge、ready、reconnecting、closing、closed、failed。events 首包必须给完整 session.snapshot，解决建连期间丢事件问题；可 GET 快照恢复观察。

错误统一为 `{v:1,error:{code,message,retryable,details?},request_id}`。不把底层错误堆栈、私钥路径或协议原始凭据返回页面。

HTTP 模板冻结在 `protocol/webservice.openapi.json`，server 变量提供产品标识与能力。写操作携带 `X-Request-ID`，同一主体同一请求 ID 的重复内容按文档定义幂等，不同内容返回 conflict；读操作可由服务器生成 request_id。OpenAPI 的 Cookie 名称模板在品牌生成时展开，不是字面大括号 Cookie。

| HTTP | 稳定错误码 |
| --- | --- |
| 400 | invalid_request、invalid_target、invalid_path |
| 401 / 403 | authentication_required / permission_denied |
| 404 | not_found（对其他主体资源统一不可见） |
| 409 | invalid_state、stale_revision、offset_mismatch、challenge_expired |
| 413 / 429 | payload_too_large / quota_exceeded |
| 422 | unsupported_feature、unsupported_key_format、codec_unavailable |
| 502 / 503 / 504 | target_connect_failed / backend_unavailable / target_timeout |

### 1.1 可选受信反代上下文

WebService 不校验 Gateway JWT。配置为受信反代后端时，使用独立本地调用方凭据认证 Connector，接受 `X-RAccess-Context` 的 base64url JSON：`{v:1,subject_ref,access_id,capability,target_id,scopes[],expires_at}`。它是与任何同机反代都可对接的通用访问约束，不含云端账号密码或企业/TURN 状态；普通独立 LAN 入口不用此头。

只有专用 listener 和已认证本地调用方能使用此上下文，其他入口拒绝或剥离，不允许浏览器伪造。业务会话捕获主体和权限交集，HTTP/WS 升级后也继续按业务动作限制。只读 desk 不接受输入、上传、剪贴板写入；业务配置禁用的功能不会被反代头重新启用。

Gateway grant 的操作范围由 Connector 转成此头并限制有效期，WebService 仍校验自身访问/目标协议策略。Connector 不解析 RustDesk/SSH 等业务 payload；没有可选访问约束适配的普通后端只能接受其明确支持的粗粒度访问，不能承诺细粒度只读授权。需细粒度限制而后端不支持时拒绝建立该会话，不静默授予全部权限。

subject_ref 在受信调用方命名空间内是不透明主体引用，用于资源隔离，不要求 WebService 同步 Gateway 用户库。上下文过期及代理连接关闭只影响关联远程业务，不终止独立 LAN 会话。

## 2. 固定数据 profile

| 能力 | stream_profile | 数据契约 |
| --- | --- | --- |
| ssh、shell | terminal.v1 | WS binary=终端字节；text=下述终端控制 JSON |
| sftp | sftp.v1 | HTTP 目录/传输资源 + events；不使用无关联 ID 的裸二进制文件块 |
| vnc | rfb.v1 | WS binary 为标准 RFB 有序字节，供 noVNC 使用，不自造图像协议 |
| rdp | rdcleanpath.v1 | RDCleanPath v3390 协商，随后 WS binary/TLS 字节桥，供固定 IronRDP Web 版本使用 |
| desk | rustdesk-message.v1 | 每条 WS binary 是一个固定快照 hbb.Message protobuf，不是整个原生 TCP/加密封装 |

各 profile 的 WS 均先完成业务资源及调用方认证。用户可直接打开局域网原生 HTTP/WS；远程由 SDK 提供等价字节/消息接口。不能为了 WebRTC 将 SFTP、VNC/RDP 解析塞进 Connector。

## 3. 业务连接配置与凭据

SSH/SFTP 共用 WebService 自有 ConnectionProfile：`id,label,host,port,username,auth_method,credential_ref,host_key_policy,jump_chain[]`。jump_chain 是已保存 profile ID 的有序列表，最多 8 跳，拒绝循环；所有跳与最终目标分别认证、校验主机密钥。失败不得悄悄直连目标。

业务页面提供自己的目标列表、最近连接与凭据交互，不迁移 Warpgate/RustTerm/RustDesk 整套 UI。配置可通过受限业务资源或本地文件管理，不要求 Connector 在线。公开描述不返回密码、私钥或口令。

业务目标资源为 `GET/POST B/api/v1/targets/` 与 `GET/PATCH/DELETE B/api/v1/targets/{target_id}/`；写操作使用业务配置管理权限，PATCH 含 expected_revision，公开响应省略秘密内容。删除正在使用的目标只阻止新会话，是否关闭旧会话通过显式 terminate_sessions 参数表达，不暗中改变其他会话。

凭据导入资源 `POST B/api/v1/credentials/`，字段为 kind=password/private_key、secret、passphrase?、persist=false。成功返回 opaque credential_ref 和 expires_at。默认会话级临时保存，加密私钥由后端解密，空口令和错误口令区分处理；持久保存需明确选择且使用本地受保护存储。对 key 文件上传使用浏览器文件内容，不接受远程用户传 WebService 任意本机路径。

host_key_policy 为 strict 或 ask；ask 时产生 challenge，包含 host、port、算法、SHA256 指纹、是否 changed 和 previous_fingerprint。允许 reject/accept_once/save；changed 不可自动接受。每跳都执行相同规则，跳板凭据与目标凭据不混用。

## 4. SSH/SFTP 功能与体验冻结

功能及交互主参考 Warpgate，底层使用 russh/russh-sftp；YoDesk 提供接入和部分实现，RustTerm 补充加密密钥/终端细节。Warpgate 已检查 v0.30.2 有跳板链和存储密钥选择，但其导入接口要求无口令私钥，不能宣称照搬就已支持加密密钥。

首个完整 SSH/SFTP 版本必须满足以下矩阵，未实现项不能标为 ready：

| 能力 | 交互与验收 |
| --- | --- |
| 密码认证 | 失败明确定位到跳板或目标，不把密码写日志，允许改凭据后新建连接 |
| 私钥登录 | Ed25519/RSA 等实际支持格式列出；私钥文件、可选 passphrase、格式及口令错误分别提示 |
| 跳板链 | 每跳独立凭据与指纹，可见当前连接阶段，任一跳失败不绕过链 |
| keyboard-interactive | 通过 challenge 提供多轮 prompts、echo 标记与取消，不能用重复密码假装完整支持 |
| 终端 | UTF-8、Ctrl-C、resize、复制粘贴、多标签独立、中文输入法、断开状态与手动重连 |
| SFTP | 目录/面包屑/隐藏文件、上传下载、进度、取消、冲突处理、续传和远程根限制 |

不把代理服务器本身作为被访问机器，不把 Connector 的系统用户凭据当 SSH 用户凭据。SSH agent forwarding、X11、会话录制及证书登录可通过 features 扩展；不能因为上游具备就标记本产品已支持。

终端 binary 是原始终端字节；text 控制格式 `{v:1,type,request_id,payload}`。客户端 `terminal.resize` 的 payload 为 cols/rows（1..1000），`terminal.close` 关闭会话。服务端 `terminal.ready`、`terminal.exit`（exit_code、signal 可 null）、`terminal.error`。键盘输入只经 binary，一次 send 的边界不代表命令边界。Ctrl-C 为 PTY 输入字节，不通过服务器拼接 shell 命令。用户终止后立即禁止输入，输出按终端顺序展示。

SFTP 路由（均在 B/api/v1 下）：

- `GET sessions/{id}/entries/?path=...&cursor=...`：单页 items + next_cursor，每条有 name/path/type/size/mtime/version；size 为十进制字符串。
- `POST sessions/{id}/entries/`：新目录；`PATCH .../entries/`：rename（from/to）；`DELETE .../entries/?path=...`：删除，递归必须显式 recursive=true。
- `POST sessions/{id}/transfers/`：direction/path/size?/conflict=fail|overwrite|rename/source_version?，返回 transfer_id、offset、state。
- `GET sessions/{id}/transfers/{tid}/`：读取服务端确认的 offset/state/source_version，以此恢复进度，不用客户端猜已落盘字节数。
- 上传 `PATCH .../transfers/{tid}/content/`：octet-stream，Upload-Offset 指明期望位置，块上限 1 MiB；匹配才写，成功 204 + Upload-Offset。重复 request_id 与相同内容返回原结果，不同内容为 conflict；客户端不得盲目重放。
- 下载 `GET .../transfers/{tid}/content/`：标准 Range/206，源版本变化返回 source_changed；读边界限制在配置远程根。
- `POST .../transfers/{tid}/complete/`：核对总大小及可选 SHA256，再完成；`DELETE .../transfers/{tid}/`：取消。覆盖/删除行为明确显示给用户，网络错误默认保留可恢复部分，取消不删除传输前已有目标文件。

events 输出 transfer.progress/completed/failed/cancelled；progress 包含 transfer_id、bytes、total、state，数字大小使用字符串，更新合并限频，不向 UI 推每个数据包。

## 5. VNC 与 RDP

VNC 使用 noVNC 自带的认证和图形协议交互，后端仅桥接允许目标。认证涉及页面输入时仍受本机业务会话权限限制；不会把 Gateway 密码发送到 VNC。

RDP 固定使用 RDCleanPath v3390 的 DER 请求/响应和 TLS 桥；destination 必须与目标配置一致，不能信任首帧的地址而绕过目标限制。TLS 证书校验采用系统信任或明确指纹/用户确认，禁止照搬旧代码的宽松 verifier。NLA/域账号、证书拒绝、登录失败和会话切换分别验收；预构建 WASM 必须记录来源及可复现构建版本。

## 6. desk：只集成 RustDesk 端侧能力

源码参考固定为 `E:/NetJobs/rustdesk` 的 `5217f32a123b100899596f77e771d0141e32e307`。不集成 rustdesk-server、rustdesk-api-server-pro，也不迁移 Flutter/Sciter UI；官方 WebClient 只作协议/交互思路参考。

所查工作区 `libs/hbb_common` 当前位于 `7e1c392c62d39c364127307cd408421dd5f8cfb0`，与主仓库声明的 `fae47e024dd83cef6aaecde0aa7d11731df11649` 不同，不能擅自 checkout/reset。应用侧 `libs/base/protos/message.proto` 才是当前业务 Message 定义，冻结为 `protocol/rustdesk/message-v1.proto` 的原字节快照，SHA256 写入 sources/lock，不拿旧 hbb_common proto 替代。

desk WS 每个 binary 完整承载一个 `hbb.Message`，字段号保持快照原义，支持 LoginRequest/Response、PeerInfo、VideoFrame、AudioFrame、MouseEvent、KeyEvent、Clipboard/MultiClipboards、FileAction/Response、Misc 等已定义消息。不额外包原生 TCP BytesCodec 长度和端到端加密握手：外层已是受认证 HTTP/WS，远程已有 WebRTC DTLS/WSS。适配器需明确接入端侧会话的身份与授权，不通过公开接口打开匿名本机桌面。

此 profile 是本产品与选定端侧适配的接口，不承诺未修改原生 RustDesk 客户端可以直接连接。不能把冻结 protobuf 视为已经能够绕过原生身份握手直接调用内部 Connection::start；T02 必须验证最小接入缝并保留功能权限检查。

WebService 负责 WS 与 worker 的消息桥，worker 运行 RustDesk 会话/采集/编码/音频/输入/文件/剪贴板等端侧功能，禁用其自动连接公共服务端和内置远程穿透。worker IPC 冻结为 Unix socket 或 Windows named pipe 的字节流：首条 length-prefixed JSON hello（worker_protocol=1、instance_id、一次性启动凭据），后续 `u32be length + protobuf hbb.Message`；1..16 MiB，启动凭据由继承句柄传入，不放命令行。上层本机控制信息另用长度前缀 JSON 控制连接，禁止与未标记 protobuf 混流。

功能目标：视频/输入、音频、剪贴板、文件、多显示器、画质/帧率、只读/控制权限与平台授权。通过 PeerInfo/Options 及 WebService features 明确交集；暂不支持的功能返回 unsupported_feature，不用旧桌面代码回填。客户端协商实际可解码 codec，不将 H265 等平台差异视为保证可用。

本地仓库说明中的 `flutter/web/js/` 实现不在工作区，不能假定官方浏览器客户端随源码可构建。本产品自己实现桌面业务页面和解码/输入适配，源码与 UI 只参考所需部分，不整体照抄。原生 protobuf 快照保留许可证与来源。

## 7. 通用 events/challenge

events 格式 `{v:1,type,event_id,business_session_id,payload}`，event_id 为当前业务会话内递增十进制字符串，不承诺断线后补发所有历史事件。客户端重连先 snapshot，再继续事件；不得重放用户输入。

challenge 具有 id、kind、expires_at、payload。kind 为 credentials、host_key、keyboard_interactive、certificate、desktop_approval。回答绑定业务 session 和当前 challenge，过期/重复/错误主体返回明确 409/404；已满足的同 request_id 相同答案可幂等返回。多轮 challenge 生成新 id，不让旧回答满足新一轮。

关闭、取消、后端退出均推进状态并释放资源。shell 连接关闭即终止 PTY/子进程；desk 断开释放其业务会话，不杀其他用户会话；独立 worker 崩溃不导致 Connector/Gateway 退出。
