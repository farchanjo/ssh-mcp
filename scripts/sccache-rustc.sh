#!/bin/sh
# rustc wrapper for sccache.
#
# sccache 0.19 exits 1 on every rustc call while `CARGO_INCREMENTAL=1` is
# exported ("incremental compilation is prohibited: Unset CARGO_INCREMENTAL to
# continue", src/commands.rs). A host that exports the variable therefore kills
# the build outright. Unset it for the wrapper's children: cargo has already
# made its incremental decision by then, so the profile still wins.
#
# Scope, so nobody expects more than sccache can deliver:
#   * `-C incremental` crates are skipped (src/compiler/rust.rs, "cannot_cache!
#     (\"incremental\")"); sccache tracks issue #236 and no release supports it.
#   * `--crate-type bin` is skipped too — rustc links those and sccache cannot
#     see the linker inputs.
#   * Everything `--crate-type lib`/`staticlib` without incremental is cached,
#     which for this crate means third-party rlib dependencies.
#
# The Makefile pins `RUSTC_WRAPPER` to this script because an exported
# `RUSTC_WRAPPER` outranks `.cargo/config.toml`.
unset CARGO_INCREMENTAL CARGO_BUILD_INCREMENTAL
exec sccache "$@"
