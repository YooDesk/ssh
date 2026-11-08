# Desktop worker IPC v1

此协议仅供同一安装目录中的 WebService 服务父进程和其创建的桌面 worker。
Windows父为Session0 SYSTEM、桌面子为目标交互Session中的SYSTEM。
Linux父为root HTTP服务，桌面/音频/输入worker为当前图形会话真实UID/GID；父只授指定UID的uinput设备权限。
只有非X11、enable_drm（默认true）及短root子逐活动CRTC实际抓帧probe通过，才起常驻DRM-only root helper。
HTTP、访问权威、业务会话、SSH/SFTP和聊天属于父进程；普通文件HTTP鉴权始终在父，
Windows文件系统调用在SYSTEM父file-only ABI，Linux文件系统调用和TransferJob在用户worker，保持用户HOME/Downloads及权限。
桌面worker提供显示器、编码帧、光标、输入、剪贴板、系统音频和隐私屏/HDR。
worker 不读取服务配置或访问密钥，不建立 HTTP/WS listener。

每个 worker 代次使用随机命名管道。仅 SYSTEM 可访问，拒绝远端客户端。
双方用 Windows 管道 API 校验对端 PID，父必须匹配其持有进程句柄的实际子 PID；
子必须匹配创建它的父 PID。PID 通过后父生成32字节一次性 nonce，子确认固定版本与 nonce。
nonce 不进命令行、环境、URL、日志或持久文件。

Linux使用随机 `/run/yoodesk-desktop-*/worker.sock`：普通桌面通道目录root:目标GID且0710，socket目标UID:GID且0600，
组成员只有目录搜索权，socket只允许目标UID/root连接，另以SO_PEERCRED验证实际子PID与预定UID。
用户worker反向验证实际父PID且父uid=0；root helper控制通道为root-only0700/0600并双向root PID校验。
两类通道均执行相同nonce握手；helper验证后只probe/运行DRM并等待EOF，不运行输入/桌面/音频/文件/HTTP分派。
用户worker中的成熟uinput服务走用户私有0700/0600通道，长寿命runtime建立客户端并按真实布局更新范围；禁DRM不禁输入。
父使用进程组与PR_SET_PDEATHSIG回收每代两类子进程；Windows使用KILL_ON_JOB_CLOSE Job。

消息由两个小端 u32（JSON 元数据长度、原始二进制长度）及其内容组成；
元数据最多4096字节，二进制最多24MiB，不使用 base64 放大媒体/剪贴板。
每条请求头包含 v=1、单调 id、op，args JSON放在有界payload；
响应回相同id，小型媒体元数据与原始媒体分离，较大的endpoint JSON亦放payload。
连接单 writer、串行请求背压，不排无界队列；媒体 poll 最多等待5ms，避免长轮询阻塞输入。
I/O 有总期限，超时/协议错误关闭本代管道，不重试写入或输入。
Unix握手与每次父请求的读写共享同一绝对10秒期限，逐块进展不重置计时；Windows父使用PIPE_NOWAIT和有限轮询共享10秒绝对期限，close设置终态取消，不把长同步内核I/O留在父进程中。Windows子同步读由父死亡监视及Job回收。
连接失效/协议损坏标记本代不健康，即使worker进程仍存活也由宿主回收并退避重建；正常业务4xx不会使连接失效。

操作白名单：displays、capabilities、input.create、input.mouse、input.key、
input.release、input.drop、subscribe、unsubscribe、unsubscribe_conn、replay_keyframe、replay_cursor、
poll、poll_cursor、poll_audio、ack_video、refresh_video、endpoint。
endpoint接受capabilities、clipboard.*、privacy.*；Linux另允许已经由父鉴权的files.*，Windows files.*仍只在父处理。
输入句柄与业务 conn 绑定当前 worker 代次，worker 更换后旧请求失败，禁止重放。
input.release 只抬起按键/按钮并保留可复用句柄（失焦、只看切换）；input.drop 在所有者销毁时释放并移除句柄、归还额度。
父子始终由同一安装产物启动，新增内部操作同步更新双方，不更改公开HTTP协议。
退出/切换/worker故障先取消旧transport I/O再关闭旧desk会话，保持父HTTP和其他协议会话可用。
Job kill-on-close 与父进程存活监视负责回收 worker 和本地隐私/输入状态。
