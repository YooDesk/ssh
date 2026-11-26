
## 2026-11-12 Android and account devices

Migrated upstream `c0975f27b05dc33dd303742cea519f390552fb4c` from the read-only `E:/YooDesk` checkout.
Synchronized account/node-control protocol snapshots; native implementation is unchanged.

2026-11-25 SFTP throughput fix: continuous HTTP upload/download with bounded 1 MiB processing; preserve read-ahead across contiguous blocks and overlap metadata checks with reads. Keep source version, ownership, permission, offset, cancellation, staging and commit checks; never replay uncertain writes. 72 unit tests passed (2 environment-dependent tests ignored), including real SFTP-protocol streaming/version/cancellation regressions. Browser package built; current API descriptions and shared transport synchronized. No release.

2026-11-26：为Gateway U38真实SFTP验收重建ssh/assets/viewers/ssh.js及LEGAL，恢复当前源码已有累计字节不超过声明大小的检查；缓存旧资产缺此检查。沿用原viewerAdapters、只打包SSH入口至实际assets/viewers，未改ABI/SSH逻辑。当前Rust库locked构建，实际Connector/WebService/隔离Paramiko SSH+SFTP/Chromium连续两轮race99.985秒完成上传、逐字节下载、删除，目录0文件；详见../gateway/.ci-tmp/batch119/source-final.json。
