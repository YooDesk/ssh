# 协议军规

先读 [START_HERE](../docs/agent/START_HERE.md)、[RULES](../docs/agent/RULES.md) 和协议 README。v1 历史资产及 history/v1 不修改；当前实施目标为 [v2 候选](../docs/design/10-revision-v2.md)，不得因 Schema 通过就宣称正式冻结或运行时互通。

修改公共字段、路由、鉴权语义或二进制布局时必须记录兼容决策、同步所有端和正反向向量；破坏变更新增主版本。yamux 是唯一多路复用方案。

RustDesk 只复用端侧协议/功能，不加入服务端或官方 UI。固定 Message 来源为 libs/base/protos/message.proto，不能替换成不同 submodule 中的同名旧文件。

正常实现本协议无需反复向用户确认。遇到源码不符合协议先写适配或明确失败，不偷偷放宽校验。验证命令为 `python scripts/check_docs.py`，它不代替真实三端互通测试。
