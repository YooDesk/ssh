# RDP产品适配（v2）

页面入口为多连接标签工作区，内部`?viewer=1&target=<公开目标ID>`承载独立同源viewer；不在URL传递凭据。
剪贴板空格式初始化必须完成标准MS-RDPECLIP握手，首个FormatListResponse不得误认为用户复制确认。
文件控件在通道Ready后启用，下载请求固定原剪贴板锁ID，换标签不能改变数据来源。

沿用现有访问认证、连接目录、SessionInfo、Challenge与单次WS票据；capability=rdp，
stream_profile=rdcleanpath.v1。目标配置新增可选domain/certificate_sha256，默认空，旧配置保持兼容。
certificate_sha256为64位十六进制叶证书摘要。服务端只连接会话创建时保存的目标，首帧destination必须完全匹配。

WS首个二进制帧为IronRDP RDCleanPath v3390 DER，请求不得含服务端响应字段或preconnection blob。
X.224协商保留客户端安全协议，不重写为SSL-only；随后TLS验签、系统证书验证或明确指纹确认，才返回证书链与X.224响应。
后续原生RDP字节在TLS流与有界WS之间转发，浏览器IronRDP完成NLA、解码与输入，不在WebService实现RDP解码。

自签名/名称不匹配证书使用冻结certificate Challenge，经HTTP轮询/回答，默认拒绝；
用户accept_once后重新连接并严格固定刚确认的摘要，第二次证书改变即失败。预配置摘要不匹配不能临时覆盖；
过期/尚未生效证书拒绝。TLS握手签名始终使用标准验证，不使用accept-all verifier。
用户名、域可保存在连接配置；密码只在浏览器内存及原生NLA中使用，不能存URL、日志或连接快照。

产品页面支持新建/编辑其他主机、本机默认127.0.0.1:3389、全屏/适应窗口/分辨率调整、只看、
键鼠、自动文本剪贴板、截图、侧边主机维护；工具栏无文本菜单、Ctrl+Alt+Del和重连按钮。
按用户最新要求，意外断线自动创建新业务会话，在当前逻辑连接内复用内存凭据和已明确接受的同一证书摘要；
主动关闭/取消/退出停止重连并清除内存凭据，证书变化仍要求确认，认证失败需修正凭据。
标准MS-RDPECLIP文件通道对应rdp.clipboard.files；文件选择/远端粘贴上传，远端复制后下载。
单块/并发/内存有界，取消/只看/断线终止传输，不跨连接重放写入；目录和磁盘重定向尚未实现。
RDPCookie页面使用原生同源Cookie认证WS；通用stream端点仍支持单次票据。不引入全局WebSocket替换。

测试部署拓扑：浏览器→Ubuntu WebService→windows-br:3389，Windows切换登录不影响Ubuntu业务进程。
