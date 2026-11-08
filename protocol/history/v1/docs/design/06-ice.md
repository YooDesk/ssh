# STUN/TURN、资源池及配置同步

状态：FROZEN-v1，网络配置字段由 control Schema 固化。STUN/TURN 使用标准协议，产品前缀只约束应用 HTTP/WS，不改写 stun:/turn:/turns: URL。

## 1. Gateway 服务能力

内置服务基于 Go Pion，不从零实现 STUN/TURN。支持完整网关、仅 STUN、仅 TURN、STUN+TURN；纯 ICE 不初始化账号数据库/签名器，不要求 Connector 或 WebService。独立 TURN 用本地 realm 和受保护密钥校验时间限制凭据。

UDP/TCP/TLS listener、relay 对外地址及端口范围分别配置。监听器和每个 allocation 都使用有界缓冲及配额；不能把“WebRTC 应用块较大”理解为必须发超大 UDP 数据报。NAT 部署必须显式设置 relay 公网地址，不能自动公布 loopback 或仅内网地址。

外部节点支持 coturn/兼容服务商。集群流量是有状态 TURN allocation，不可用 HTTP 反代轮询分发；采用保持分配归属的四层方案或显式节点列表。多个外部节点独立配置认证密钥/供应商凭据签发器。

## 2. 运行期数据模型

| 对象 | 字段 |
| --- | --- |
| IceNode | id、kind=stun/turn、provider=embedded/external、urls[]、region、enabled、health、auth_ref、capacity |
| TurnPool | id、owner_type=platform/user/enterprise、owner_id、node_ids[]、quota、enabled |
| IcePolicy | id/revision、allowed_stun_ids[]、primary_pool、backup_pools[]、allow_shared_fallback、allow_wss、relay_only |
| AllocationRecord | session_id、policy_revision、pool/node、endpoint_role、credential_expiry、path、bytes/错误摘要 |

节点秘密与公开 URL 分离，API 返回 configured 标记和密钥引用，不返回共享密钥。独立节点不持有全部账号/企业数据。节点 ID/池 ID 不能由浏览器直接指定后获得凭据。

## 3. 策略决策顺序

1. 按目标设备所属企业/工作空间确定企业策略，访问者个人 VIP 不覆盖企业限制。
2. 个人设备会话按发起用户已生效 VIP 权益选其专用池；没有专用绑定时按权益配置共享或拒绝，不自动假设有专用节点。
3. 普通个人会话使用平台共享池。

先计算允许节点集合，再结合健康、区域和容量选择，不在全局目录先挑最快节点再补权限。默认允许直连；TURN 专用意味着需要中继时走指定池。企业 relay_only 模式对两端设置 relay 策略，Connector 校验可接受候选/实际路径，并禁止未授权公共 TURN/WSS 回退。

专用池默认不降级到公共池；允许备用专用池、共享池和 WSS 是三个独立授权选项。没有可用获准路径时返回 policy_denied 或 backend_unavailable，不泄露其他租户节点。

多个 ICE server 的 URL 顺序不是严格主备。严格优先级由 Gateway 分批下发获准节点，失败后显式新协商代次；不能沿用旧代码的 weight 排序就宣称确定路由。

## 4. 心跳设备配置

注册和 heartbeat_ack 可携带 network_config。字段固定：revision、valid_until、stun_servers[{urls[]} ]、ice_policy_revision。设备默认配置不含 TURN 密码和全局节点目录。

- C 首次或重连报告空 applied_network_revision，G 返回当前完整快照。
- G 对比**已应用版本**，有变化或临近失效就重发最新快照；未变时省略字段。
- C 验证结构、身份范围、URL、有效期后原子替换；成功才回报 applied；失败保留原有效版本并回报 failed/error_code。
- 省略 network_config 表示保留，空 stun_servers 表示清除；不可偷偷补公共默认 STUN。
- revision 是不透明版本字符串；每次配置有效期也变化时产生新 revision。旧代次或旧作用域快照拒绝应用。
- 过期不新建依赖该配置的 PC；通过控制连接请求更新，或使用会话已批准的 WSS，不自行选择其他资源。
- G 可主动发 network_config_changed，C 立即心跳；即使通知丢失，也由周期同步收敛。

配置应用/拒绝与网络配置有效期有单独指标。退出、切换 Gateway origin 或归属后清旧缓存，不复用另一个企业的 ICE 列表。

## 5. 会话 ICE 配置

ice_config 必填：revision、policy_revision、expires_at、ice_transport_policy=all/relay、allow_wss、ice_servers[]。每项为 `{urls,username?,credential?,credential_expires_at?}`。stun 不要求 credential；turn 必须具备短期认证，分别为浏览器和 Connector 签发。

创建会话时此快照**完整覆盖**设备级默认值，不作列表并集。G 在 session_open 和浏览器创建会话响应分别下发；内置和外部服务通过相同模型处理。凭据不必是同一个用户名，必须在同一获准策略内。

凭据 TTL 不等于远程会话租约；提前刷新由会话定时器触发 ice_config_update，不能等慢心跳才续。更新不自动保留过期权限，需在必要时 ICE restart/重建；失败按会话策略回退。撤销直接关闭会话，不等待 TURN 凭据自然过期。

短期 TURN 凭据只能限制使用时段及服务端配额，不能假装标准 TURN 能强制绑定应用 session_id。G 保存分配映射，C 仍验证业务 grant；节点按用户名/配置执行并发与带宽配额。

## 6. 支持矩阵与验收

Gateway 提供 TURN TCP/TLS 与 Connector 能通过 TCP/TLS 使用 TURN 是两回事。已检查 YoDesk 的 Rust 0.21.1 适配主动跳过 turns/tcp；T01 必须实际验证所选 Rust 组合。若某传输未实现，报告限制并使用获准 WSS，不能更改 URL 为 UDP 后说成功。

WebRTC 直连、TURN/UDP 和 WSS 为首版必需；TURN/TCP/TLS 服务端必须有标准客户端验证，Connector 侧支持情况单独登记，补齐路径不能通过删除 WebRTC 需求完成。真实禁 UDP 环境、外部多节点和 relay_only 专用池分别验收。

健康探测执行 STUN binding、TURN allocate/permission/channel 数据往返与清理；不仅测端口。TURN 验收读取两端 selected candidate pair，有 relay 候选不是验收证据。直连测试需要区分同 LAN host 和跨 NAT srflx。
