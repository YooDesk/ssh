
## 2026-11-12 Android and account devices

Migrated upstream `c0975f27b05dc33dd303742cea519f390552fb4c` from the read-only `E:/YooDesk` checkout.
Synchronized account/node-control protocol snapshots; native implementation is unchanged.

2026-11-25 SFTP throughput fix: continuous HTTP upload/download with bounded 1 MiB processing; preserve read-ahead across contiguous blocks and overlap metadata checks with reads. Keep source version, ownership, permission, offset, cancellation, staging and commit checks; never replay uncertain writes. 72 unit tests passed (2 environment-dependent tests ignored), including real SFTP-protocol streaming/version/cancellation regressions. Browser package built; current API descriptions and shared transport synchronized. No release.
