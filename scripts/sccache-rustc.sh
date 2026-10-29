#!/bin/sh
# rustc wrapper for sccache.
#
# sccache 0.19 aborts every rustc call while `CARGO_INCREMENTAL=1` is exported
# ("incremental compilation is prohibited"), yet `[env]` in `.cargo/config.toml`
# is the only way to keep cargo incremental on hosts that export
# `CARGO_INCREMENTAL=0`. Unset the variable for the wrapper's children: cargo
# has already made its incremental decision by now, so the build stays
# incremental while sccache keeps caching.
#
# `-C incremental` stays in the cache key, so an incremental session misses the
# cache instead of poisoning it; release (`incremental = false`) neither passes
# the flag nor the variable.
unset CARGO_INCREMENTAL CARGO_BUILD_INCREMENTAL
exec sccache "$@"
