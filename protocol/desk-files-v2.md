# desk 文件位置与传输扩展 v2

文件管理追加（2026-11-01）：同一endpoint增加以下严格动作，root_id仍可省略。

| 请求 | 语义 |
| --- | --- |
| `{action:"files.create",path,kind,root_id?}` | 创建空文件(kind=file)或单层文件夹(kind=directory)，重名409 |
| `{action:"files.rename",path,name,root_id?}` | 原目录内重命名，name仅单个名称；系统原子禁止替换，重名409 |
| `{action:"files.delete",path,kind,root_id?}` | 永久删除文件或递归删除文件夹；类型变化409，界面确认名称及文件夹全部内容 |

成功为`{applied:true}`。写动作均受服务端只读限制(403)，根路径禁止操作，沿用相对路径、
symlink/reparse point和内部临时目录保护。活动上传/下载文件及祖先目录禁止改名/删除(409)。
不存在404，无权限403，其他原生失败503。创建/改名不自动编号；Windows用MoveFileExW无替换标志，
Linux用renameat2 RENAME_NOREPLACE，拒绝竞态覆盖。旧客户端保持原动作，UI与原生库整体部署/回滚。
确认框绑定原目录/会话/输入代次，切只看或断开关闭；已发送的原子修改不能撤销。
百分比来自已确认字节，两种方向均可逐项取消；最终提交/交给浏览器保存阶段不可撤销。

最新UI调整（2026-11-01）：当前路径各层可点击返回，Windows最前层为“我的电脑”；
移除独立系统根快捷按钮，增加“默认存储位置”按钮（使用root_id=default）。
祖先位置由完整显示路径生成，仍请求枚举的系统根和根内相对路径；此调整不改变API。

同名上传追加：沿用现有files.upload/files.commit请求；没有新增动作或请求字段。
现有成功响应补充`name`（最终文件名），旧客户端可忽略该附加字段。
同名按Windows式编号自动另存，已有目标保留；提交时发生的新冲突也继续编号。

这是现有 `desk-endpoint-v2.schema.json` 的兼容扩展。请求/响应继续经过同一个会话 endpoint，
保留认证、Origin、CSRF、X-Request-ID、所有权和只读检查。

初版用户于 2026-11-01 明确要求简化位置入口：显示当前完整目录，快捷打开系统根；
Windows 先显示“我的电脑”，再选择 `C:\`、`D:\` 等磁盘。此次系统根浏览是明确的功能范围变更。

| 请求 | 成功 data |
| --- | --- |
| `{action:"files.roots"}` | `{platform:"windows"或"unix",roots:[{id,path}]}`；Windows 使用 GetLogicalDrives 枚举磁盘，Unix 返回 `/` |
| `{action:"files.list",path,root_id?}` | `{root,path,platform,entries:[{name,type,size}]}`；root/path 均为供显示的完整路径 |
| `{action:"files.upload",path,size,root_id?}` | `{transfer_id,size,name}`，后续 write/commit 不再携带位置 |
| `{action:"files.commit",transfer_id}` | `{committed:true,name}`，name可能在提交冲突时再次改变 |
| `{action:"files.download",path,root_id?}` | `{transfer_id,size}`，后续 read 不再携带位置 |

`root_id` 只能为 `default`、`system`、`drive-A` 至 `drive-Z`，并且必须实际由当前平台枚举。
省略或使用 `default` 时，仍使用原 `YOODESK_DESK_FILE_ROOT`/默认 Downloads/YooDesk 目录。
`system` 只用于 Unix；Windows 使用对应磁盘 ID。“我的电脑”是前端的虚拟列表，不是可写路径。
`path` 始终是选中根内的相对路径，空串仅用于列出根。绝对路径、`..`、内部临时目录、
symlink 和 Windows reparse point 校验继续执行；访问权限由服务进程的操作系统身份决定。

无效字段/根/路径返回 400；不存在或无权限的目录如实返回原生错误。
单文件仍限制 1 GiB，每会话最多四个服务端任务；UI 最多排队 32 项并逐项执行，记录最多 40 项。
同名上传分配`名称 (2).扩展名`、`名称 (3).扩展名`，保留已有文件；未提交任务也占用文件名。
扩展名、无扩展名和中文保持原规则；名字已有编号时继续递增。临时文件位于目标父目录，保持原子、不覆盖发布，
并避免浏览 `/` 时必须有写入磁盘根的权限。取消和会话关闭仍回收临时目录。

队列绑定选择时的根、目录、会话和输入代次；失败只允许用户主动重试，不自动重放写入。
创建请求返回前也占据队列，晚到的已取消创建只发送回收请求。只看模式不发新的文件操作。
下载完成提示“已交给浏览器保存”，不声称浏览器已经写入磁盘。

UI 和原生桥随同一个 WebService 产物发布；新 UI 依赖此次位置扩展，回滚应回滚整个产物。
旧客户端不传 root_id 的请求保持原目录语义。此扩展不增加覆盖、目录上传或断点续传。
