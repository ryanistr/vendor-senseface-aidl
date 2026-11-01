# Third-party licenses

The FaceHAL source in this repository is licensed under Apache-2.0 (see `LICENSE`).
It links the following crates, each under a permissive license. The versions and
license identifiers below come from each crate's own `Cargo.toml`, and
`Cargo.lock` pins the exact revisions the build uses.

| Crate | Version | License |
|---|---|---|
| `async-trait` | 0.1.92 | MIT OR Apache-2.0 |
| `bitflags` | 2.13.2 | MIT OR Apache-2.0 |
| `bytes` | 1.12.1 | MIT |
| `cfg-if` | 1.0.5 | MIT OR Apache-2.0 |
| `downcast-rs` | 2.0.2 | MIT OR Apache-2.0 |
| `errno` | 0.3.14 | MIT OR Apache-2.0 |
| `libc` | 0.2.191 | MIT OR Apache-2.0 |
| `linux-raw-sys` | 0.12.1 | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT |
| `lock_api` | 0.4.14 | MIT OR Apache-2.0 |
| `log` | 0.4.34 | MIT OR Apache-2.0 |
| `mio` | 1.2.4 | MIT |
| `parking_lot` | 0.12.5 | MIT OR Apache-2.0 |
| `parking_lot_core` | 0.9.12 | MIT OR Apache-2.0 |
| `pin-project-lite` | 0.2.17 | Apache-2.0 OR MIT |
| `pretty-hex` | 0.4.2 | MIT |
| `proc-macro2` | 1.0.107 | MIT OR Apache-2.0 |
| `quote` | 1.0.47 | MIT OR Apache-2.0 |
| `rsbinder` | 0.6.0 | Apache-2.0 |
| `rsproperties` | 0.6.0 | Apache-2.0 |
| `rustix` | 1.1.6 | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT |
| `scopeguard` | 1.2.0 | MIT OR Apache-2.0 |
| `signal-hook-registry` | 1.4.8 | MIT OR Apache-2.0 |
| `smallvec` | 1.16.4 | MIT OR Apache-2.0 |
| `socket2` | 0.6.5 | MIT OR Apache-2.0 |
| `syn` | 3.0.9 | MIT OR Apache-2.0 |
| `thiserror` | 2.0.21 | MIT OR Apache-2.0 |
| `thiserror-impl` | 2.0.21 | MIT OR Apache-2.0 |
| `tokio` | 1.54.1 | MIT |
| `tokio-macros` | 2.7.0 | MIT |
| `unicode-ident` | 1.0.27 | (MIT OR Apache-2.0) AND Unicode-3.0 |
| `zerocopy` | 0.8.63 | BSD-2-Clause OR Apache-2.0 OR MIT |
| `zerocopy-derive` | 0.8.63 | BSD-2-Clause OR Apache-2.0 OR MIT |

`rsbinder` and `rsbinder-aidl` are by hiking90 (https://github.com/hiking90/rsbinder).
`rsbinder-aidl` is a build dependency and does not end up in the binary.

None of these licenses impose a stronger obligation than Apache-2.0, so the
combined work can stay under Apache-2.0.

The binary also loads, at runtime and not at link time, the libraries listed in
the README's Legal section. Those ship in the device's vendor partition and are
covered by their own licenses, which are not reproduced here.
