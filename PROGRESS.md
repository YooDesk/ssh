
## 2026-11-12 final Android/device migration acceptance

Migrated upstream `c0975f27b05dc33dd303742cea519f390552fb4c` with the standalone repository boundaries preserved.
- build.yml: success; release `v0.1.0-ci.36`; run https://github.com/YooDesk/ssh/actions/runs/43198241824; tested commit `7e2d048c06e48f9cc6b2d6c595f037f07b59a7b0`.
- build-android.yml: success; release `android-v0.1.0-ci.1`; run https://github.com/YooDesk/ssh/actions/runs/43200625800; tested commit `b7538a219e0ea1f140c4779746d1abcc6c8621a6`.
Android builds use ARM64, API 24 and NDK 26.1.10909125 with 16 KiB ELF load alignment. Actual Android-device acceptance remains pending.
Original repository visibility and workflow activation were restored after the build. Documentation-only acceptance commits do not change the tested executable source.
