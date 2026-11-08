# Repository rules

This repository owns its plugin implementation (or the webservice host), workflow and release archive.
Use CodeGraph first if a `.codegraph/` directory exists. Do not initialize one automatically.
Keep the C plugin ABI versioned. Never pass Rust objects, trait objects or allocators across libraries.
Host authentication callbacks must remain authoritative; plugin authentication failures close access.
Keep Linux builds on Ubuntu 18.04 and validate every shipped ELF against glibc 2.27.
Run the relevant Cargo unit tests and the host's dynamic_plugins regression when changing the ABI or proxy.
Reference checkouts are read-only. Apply upstream adapters only inside ignored artifacts/deps worktrees.
