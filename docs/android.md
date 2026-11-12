# Android ARM64

Dispatch `.github/workflows/build-android.yml` to build this independent plugin for
Android API 24 with pinned NDK 26.1.10909125. The workflow runs the Cargo library
unit suite, builds the browser viewer assets, cross-compiles the native shared
library, validates Bionic dependencies and 16 KiB page alignment, and publishes
`yoodesk-ssh-android-arm64.zip` plus SHA256 in a separate Android release.
The Android installer pins this release together with Connector, Desk and WebService,
checks every file hash, and includes this plugin and its viewer assets in the APK.
The host continues to own authentication through the unchanged C plugin ABI.
