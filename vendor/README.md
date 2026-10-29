# Vendored crates

## Haiku Vulkan support

`wgpu`, `wgpu-core` and `wgpu-core-deps-windows-linux-android` 30.0.1 are copied from crates.io
(via PhotoCraft's `vendor/`), with their upstream MIT/Apache-2.0 licence files. The only changes
add `target_os = "haiku"` alongside FreeBSD in the platform dependency and backend cfg lists
(`Cargo.toml`, `Cargo.toml.orig`, `build.rs`), so their existing Vulkan backend builds for the
air/OS host's GPU renderer (`apps/wordcraft-haiku/src/gpu.rs`). Other platforms build the same
code as before. Remove the copies once wgpu supports Haiku upstream.
