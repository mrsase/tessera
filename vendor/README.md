# Vendored crates

`xattr/` is xattr 0.2.3 (MIT/Apache-2.0, see its LICENSE) with one fix: current
`libc` no longer defines `ENOATTR` on Linux, where the same condition is
`ENODATA`. Upstream 0.2.3 is the last release of that series, and a transitive
dependency of GPUI's HTTP client requires it, so the workspace patches it in
`Cargo.toml`. Remove the patch once the dependency graph allows xattr 1.x.
