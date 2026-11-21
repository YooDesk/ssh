# Local ABI correction

Upstream: crates.io libc 0.2.191, archive SHA256
`65cd87c9a6d60536580b316ad985726104730a5151c99845f2490428ef91240a`.

The Linux UAPI socket and glibc sigaction, signal-number, socket-type and
NPTL mutex architecture selectors duplicated mips/mips32r6 and omitted mips64/mips64r6. Correct those
five selectors so 64-bit MIPS uses its own kernel ABI: SOL_SOCKET=0xffff,
SO_REUSEADDR=4, SOCK_STREAM=2 and the MIPS signal structures/numbers. Other
target selections and all upstream license/provenance files are unchanged.
The extracted Connector startup smoke reproduces the previous ENOPROTOOPT
and verifies the corrected actual service, not just compilation.
