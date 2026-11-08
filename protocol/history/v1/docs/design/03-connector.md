# Connector 详细设计

Rust + Tokio + Axum。Connector 是设备远程接入与本机管理程序，不承载 SSH、SFTP、桌面等业务，不依赖 WebService crate。跨端消息严格遵守 [冻结协议](05-protocol.md)。

## 1. 模块与所有权

| 模块 | 唯一所有状态 |
| --- | --- |
| auth/controller | 账号授权事务、刷新/ACK、退出标记、account_epoch |
| credentials | product+Gateway origin+device_id 范围的受保护凭据 |
| directory | 已授权可见设备的完整快照、分页及更新时间 |
| control | 当前 Gateway WSS、设备连接代次、控制单写入任务 |
| network_config | 已应用快照、版本/有效期，不保存会话外全局 TURN 密钥 |
| sessions | 每个远程 session 的授权、PC/内层 yamux、租约、代次 |
| data_pipe | 设备数据 WSS 与外层 yamux、流分发 |
| backend | 固定 HTTP/WS 后端映射、路径白名单、可选发现/管理接口 |
| admin | loopback 页面、动作队列及脱敏状态，不自管云端凭据 |

阻塞凭据读写/PTY 等不在控制循环中直接执行；账号与网络 actor 通过命令/快照通信，不持锁跨 await。关闭时先阻止新资源，再取消/等待有限时间，不遗留独立运行的写任务。

## 2. 设备与账号状态机

```text
unbound → authorizing → saving → authorized → connecting → online
                         ↑                    ↓
                  waiting_network ← reconnecting
任意已授权状态 → logout_pending → unbound
```

账号已授权、设备控制在线、后端 ready 是三个不同维度。目录错误不改变账号归属，WebService 停机不退出账号，网络抖动不立刻抹掉安全存储。

参考 UniRemoteLite 后台控制器：PKCE verifier/公钥绑定、受控领取、稳定刷新 request_id、先持久化新 refresh 再 ACK；恢复时优先完成未 ACK 的事务。退出先落 tombstone、递增 account_epoch、关远程，再远端撤销，不能由晚到响应恢复。

系统凭据库不可用时采用受权限保护的加密文件方案，明确记录存储失败并保留最后有效版本，不明文降级。设备 ID 生成后持久化读回，不随登录、重连或品牌显示名变化。

## 3. 控制连接与配置同步

使用 Gateway 配置的 HTTPS/WSS 信任链和固定 origin；不接受 Token 自报的 JWKS URL，不向重定向目标发送凭据。Origin 改变后旧凭据范围失效，需明确重新授权。

注册后持有 connection_generation；心跳 15 秒，设备租约 45 秒。所有请求与返回关联 request_id 和代次；旧连接响应不能进入新 actor 状态。data WSS 同样绑定当前控制身份。

网络配置 atomic replace；只有验证应用成功才报告 applied revision。失败保留旧版本、有限错误码；到期不新建依赖旧策略的 ICE 会话。会话 ICE 完整覆盖设备默认 STUN，配置更新不强行重建所有 PC。

设备目录每轮完整分页后提交，循环游标/超限/账号变化时丢弃本轮，不用半页覆盖旧目录。离线缓存带 stale/updated_at，不能用于访问授权。

## 4. 后端与能力

后端配置包含 backend_id、固定 loopback URL、允许业务路径、调用方凭据引用及静态 capabilities。目标 SSH/VNC 地址与凭据归 WebService，Connector 不解析。

能力快照静态配置或可选发现接口均可；上报 supported/enabled/ready/protocols。静态 enabled 不等于 ready，基本健康请求失败时不继续发布可用状态。未知能力 ID 透传，不在 forwarding 核心 switch desk/ssh。

WebService 管理适配缺失时转发仍可工作。Connector admin 显示配置不可管理，不伪造开关成功；WebService 可独立从自身文件/业务接口配置。

## 5. 本机 admin 契约

固定在 `P/admin/`，仅本机 loopback HTTP；页面和 API 使用本机管理会话、Origin/CSRF、Host 校验。响应不能出现 access/refresh、设备私钥、TURN 密钥或完整目标密码。

| 路径（相对 P/admin/api/v1） | 内容 |
| --- | --- |
| `GET status/` | 账号/连接/能力/后端/网络配置状态快照 |
| `POST actions/` | login、cancel_login、logout、refresh_devices；request_id 幂等，动作主体/epoch 固定 |
| `GET actions/{id}/` | pending/succeeded/failed 及脱敏结果 |
| `GET devices/` | 当前目录快照，不分页查询云端秘密 |
| `GET/PUT settings/` | Connector 自有配置，写入 expected_revision 防覆盖 |
| `GET backends/` | 后端健康与可选管理能力 |

账号登录动作返回公开 Gateway 授权 URL，由当前用户浏览器打开；不运行 Qt、不收 Gateway 账号密码。管理 API 永不注册到业务 listener，不能只用路径黑名单作为唯一隔离。

## 6. PC/多路复用生命周期

每个浏览器远程会话独立 PeerConnection 和内层 yamux。PC 可共享引用，事件接收者由单一驱动拥有；不能用长时间持锁 wait 阻塞 offer/candidate。DataChannel 接收单消费，转换为 futures I/O；yamux 需要持续 poll 驱动。

复用 YoDesk 的帧上限、背压和 WakeOnWriteReady 回归，不能用定时主动唤醒或放大队列掩盖死锁。WSS 只有一个写任务，认证、信令和心跳共用控制发送器；大文件在独立数据连接。

会话到期/撤销关闭其 PC、yamux、HTTP/WS 后端流；Gateway 断线不关闭 WebService 独立局域网会话。只撤销当前远程上下文，禁止调用“踢掉所有桌面用户”代替局部关闭。

## 7. 默认故障行为

Gateway 不可用退避重连，上限 30 秒并加随机抖动；不阻塞 admin。WebService 不可用时控制连接可保留并上报 not_ready，新流返回 backend_unavailable。后端恢复后检查并更新能力，不自动重放旧 shell 输入/POST。

日志关联 device_id/session_id/generation/path；profile 只记录名称、版本与错误码，避免完整 SDP、凭据或键盘数据进入常规日志。临时凭据关闭时清理，未完成刷新事务安全保留以支持崩溃恢复。
