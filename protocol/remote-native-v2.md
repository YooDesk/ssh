# VNC、SSH 与 SSH 内嵌文件接口

VNC目标的username为可选默认用户名；密码仅在服务端协商需要时输入，不进入目标快照。
客户端按服务器认证类型提交凭据，标准VNCAuth不携带用户名。切换目标或修改配置清除旧内存凭据。
连接目录完整持久化，自建连接可从列表直接删除；删除当前目标终止对应会话，默认本机项受保护。

2026-11-01用户追加：`connection-targets/`允许具有targets.manage的身份POST新增配置，
`connection-targets/{id}/`支持PATCH/DELETE；均需精确Origin/CSRF/request-id。
写DTO包含label/host/port/username?/root?/host_key?/jump_chain?，PATCH另需expected_revision；
DELETE正文为expected_revision。公开摘要增加revision/is_default/root/host_key，均不含口令/私钥。
两个内建local-vnc/local-ssh固定首位、host固定127.0.0.1且不可删除；新连接可指定其他地址。
持久文件connection_profiles_file默认位于配置目录的产品ID.connections.json，存在时为完整配置快照。
会话创建仍只接收已保存target_id，不能借会话正文注入地址；修改不改变已存在会话的快照。

本产品适配沿用业务 v2 的 Session/Credential/StreamTicket/Challenge/Directory/Transfer DTO，
原生字节分别为 rfb.v1、terminal.v1，不修改 desk、Gateway 或 Connector 协议。

## 配置与发现

WebService连接目录包含两个内建本机连接及最多64个其他连接，字段为 id/label/protocol/host/port；
SSH另有 username/host_key/root/jump_chain。指纹精确比较，未配置时人工确认，
已配置却不匹配直接拒绝。最多8跳，每跳分别验证与认证，不绕过失败跳板。

产品辅助发现端点 `GET B/api/v2/connection-targets/`，B是
`/{product_id}/webclient/{vnc|ssh}`，返回 `{items:[{target_id,label,host,port,username,jump_chain}]}`。
它与冻结ConnectionProfile的targets CRUD分开；管理用户可通过本产品页面新增/编辑/删除目标，
普通业务会话正文仍不能提交地址或服务器私钥文件路径。

## 会话与认证

创建/查询/关闭/票据/WS沿用业务v2；创建201、关闭204。创建按访问会话、请求编号及规范化
正文摘要去重，结果保留24小时，最多1024条；活跃连接另受max_sessions限制。
当前仅支持空options/client_features，其他协商选项明确拒绝。

VNC先transport_ready，noVNC握手后服务器仍保持target_authentication=unknown，
不从TCP连接成功推断已完成认证。raw RFB桥要求screen.view及两种输入权限；
受限只读身份直接拒绝建流。viewer的只看是已授权控制会话的操作模式，不替代服务端ACL。

SSH创建后逐跳异步握手，认证完成才提供stream_url。五分钟临时凭据经credentials导入，
按访问会话隔离；persist=true明确拒绝。加密私钥在阻塞任务解密，不写URL、日志或浏览器存储。
终端与SFTP在最后一条SSH连接上开不同通道；子系统不可用时文件面板明确报错。

辅助 `GET sessions/{id}/challenges/` 返回 `{challenge:Challenge|null,error:string|null,sftp_available:boolean}`，
回答仍用冻结challenges/{cid}/responses。校验挑战随机编号、kind、当前访问会话和120秒期限；
交互认证最多32轮，每轮最多32个带编号/echo的提示。host_key支持reject/accept_once，
save明确拒绝并提示将已验证指纹写到服务端配置。取消终止握手，不复用旧回答。

WS仅回选binary或terminal.v1，不回显票据；坏票据禁止回退Cookie，校验精确Origin。
每个业务流只连接一次；浏览器在意外断线后自动创建新业务会话恢复，不重放输入/写文件。
凭据与接受过的相同主机指纹仅在当前逻辑标签内存中保留，关闭/取消/退出清除，交互验证码不重放。
每秒复核访问会话；
退出立即关闭该身份的连接/凭据，其他浏览器不受影响。VNC未接流30秒回收。

## 文件组合

SFTP不另设页面/会话；目录和传输放在SSH父会话的entries和transfers路径下，DTO复用冻结v2定义。
目录每页128项，读写分别要求files.read/files.write，写请求还校验Origin/CSRF/request-id。
路径按远端POSIX语义和canonicalize限制在配置根，拒绝..、反斜杠、NUL和symlink越界。

上传使用独占临时文件，块不超过1MiB，Upload-Offset匹配后写；最后确认块相同内容可重试。
完成真实计算SHA256、标准SFTP rename发布，不覆盖既有文件。取消只删除本次临时文件。
每连接最多8个活跃传输，UI最多排队64项；递归删除最多处理10000节点。
下载使用有界Range/206并复核源版本。小文件浏览器缓存上限128MiB，更大文件要求流式保存API。

尚无跨SSH重连断点恢复、覆盖/自动编号、持久密码/私钥及其他写操作的跨重启幂等日志；
指纹可在编辑连接中保存；认证挑战本身仅提供一次接受，不会自动改持久配置。
本切片不等同于T10完整退出条件。

## 共享访问CSRF轮换与写请求

登录、session读取和续期成功响应增加`X-Access-Instance`头（64位十六进制，非凭据）。
同一访问会话续期保持标识不变，新登录使用新的标识。新版viewer将操作绑定发起时的
标识：每次session读取必须确认标识一致，写请求携带`X-Access-Instance`。
服务端先比较此头与Cookie/Bearer实际认证的会话，不匹配则403且不执行业务、不重试。
该头与CSRF均需验证，不能用标识代替认证或授权。旧客户端不带此头仍按既有权限校验；
新版客户端发现旧服务端未返回标识时明确拒绝建立可写访问，不猜测会话身份。
HTTP JSON/Schema不增加字段，响应头仅供同源viewer读取。

所有viewer（含desk）仅在服务端返回HTTP400、`error.code=invalid_request`、
`error.retryable=true`、`error.details.stage=csrf`、`error.details.field=x-csrf-token`
时，才可读取当前访问session并以原X-Request-ID重试一次。该标记只由业务执行前的
CSRF校验产生；网络中断、超时、普通400和业务错误均不自动重放。
这是既有ErrorDetails字段的兼容性补充；旧客户端仍按400失败，新客户端遇到旧服务端
未标记错误也直接失败。不接受过期CSRF、不延长绝对期限、不新增路由或字段。
续期请求之间可使用有期限、可取消的同源锁，业务请求不持有该锁。
desk最后一条已建立画面流断开后关闭对应业务会话并释放输入/采集；恢复创建新会话。

正反向向量（前端与HTTP回归共同覆盖）：

| 响应 | 客户端处理 |
| --- | --- |
| 400 / invalid_request / retryable=true / stage=csrf / field=x-csrf-token | 刷新session，保留X-Request-ID，仅重发一次 |
| 同一请求第二次收到上述拒绝 | 报错，不再重发 |
| 普通400 / 无stage或field标记 | 报错，不重放 |
| 网络异常、请求取消、超时 | 报错，不重放 |
| session响应标识与原操作不同，或服务端403标识不符 | 取消旧操作，不重试到新会话 |

## 多显示器

noVNC的ExtendedDesktopSize解帧保留所有screen的id/坐标/尺寸，公开screens事件和selectScreen。
单屏选择复用noVNC Display视口、scale及absX/absY，全部显示器保留完整framebuffer。
屏幕移除会清除旧选择；多屏时禁止把整个远端布局改成单屏的resize请求。
服务端不公布布局时只提供合并视图，不推测显示器数量。
