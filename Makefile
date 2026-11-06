# Dev entry points — the single path for building and testing this repo.
#
# Everything runs through here so the knobs stay in one place:
#   * JOBS defaults to the CPU count; exported as CARGO_BUILD_JOBS and passed
#     to nextest as --test-threads. Override per run to cap cores: `JOBS=4 make test`.
#     A value above the CPU count is clamped to it; a non-integer is an error.
#   * CARGO_INCREMENTAL is stripped because it outranks every profile;
#     `Cargo.toml` decides instead (dev/test incremental, release not).
#   * sccache is pinned to the repository wrapper. A host-exported
#     `RUSTC_WRAPPER` outranks `.cargo/config.toml`, which would bypass the
#     wrapper's `CARGO_INCREMENTAL` unset and abort sccache 0.19 — so the
#     wrapper is forced here and `make RUSTC_WRAPPER=...` still wins.
#   * The en-US locale comes from `[env]` in `.cargo/config.toml`; MAKEFLAGS
#     is dropped so cargo does not warn about a closed jobserver fd (JOBS wins).
#   * `make release` installs into $(BINDIR). It defaults to /usr/local/bin, so
#     use `sudo make release` or `PREFIX=$HOME/.local make release`.
#   * `NATIVE=1` appends `-C target-cpu=native` for the release path only.
#   * On macOS the release binaries are ad-hoc codesigned before install. Pass
#     a Developer ID to sign properly: `CODESIGN_IDENTITY="Developer ID ..."`.
#   * `make all` is the whole pipeline: gates + release + install + verify.
CARGO ?= cargo
PREFIX ?= /usr/local
BINDIR := $(PREFIX)/bin
BINARIES := ssh-mcp ssh-mcp-stdio ssh-mcp-tail
UNAME_S ?= $(shell uname -s 2>/dev/null || echo unknown)
CODESIGN ?= codesign
CODESIGN_IDENTITY ?= -
# `nproc` is coreutils, absent from a stock macOS; fall back to sysctl there so
# the cap still means something. Both missing means one job.
CORES := $(shell nproc 2>/dev/null || sysctl -n hw.ncpu 2>/dev/null || echo 1)
JOBS ?= $(CORES)
RUSTC_WRAPPER = $(CURDIR)/scripts/sccache-rustc.sh

# `NATIVE=1 make release` adds `-C target-cpu=native` on top of the release
# profile. Off by default so the binaries stay portable across machines; on
# means a full rebuild, because the flag is part of the sccache key.
NATIVE ?= 0
ifeq ($(filter 1 yes true,$(NATIVE)),1)
export RUSTFLAGS := $(strip $(RUSTFLAGS) -C target-cpu=native)
endif

# JOBS must be a positive integer, and never spawn more than there are cores.
JOBS_VALID := $(shell printf '%s' '$(JOBS)' | grep -qE '^[1-9][0-9]*$$' && echo 1)
ifeq ($(JOBS_VALID),)
$(error JOBS must be a positive integer, got '$(JOBS)')
endif
ifeq ($(shell [ "$(JOBS)" -gt "$(CORES)" ] && echo clamp),clamp)
$(warning JOBS=$(JOBS) exceeds $(CORES) cores — clamping to $(CORES))
JOBS := $(CORES)
endif

export CARGO_BUILD_JOBS := $(JOBS)

RUN := env -u CARGO_INCREMENTAL -u CARGO_BUILD_INCREMENTAL -u MAKEFLAGS -u MFLAGS \
	RUSTC_WRAPPER=$(RUSTC_WRAPPER) $(CARGO)

.PHONY: info dev check all release release-min sign install verify test test-integration ci-test fmt lint doc tools-export tools-export-check clean

# Show the effective knobs before a long run.
info:
	@echo "jobs=$(JOBS)/$(CORES) cargo=$(CARGO) bindir=$(BINDIR) identity=$(CODESIGN_IDENTITY) wrapper=$(RUSTC_WRAPPER) native=$(NATIVE) incremental=dev/test-on release-off"

# Full pipeline: gates, release build, codesign, install, verify.
all:
	$(MAKE) --no-print-directory fmt
	$(MAKE) --no-print-directory lint
	$(MAKE) --no-print-directory test
	$(MAKE) --no-print-directory release
	$(MAKE) --no-print-directory verify

# Incremental, unoptimised build.
dev:
	$(RUN) build

check:
	$(RUN) check --all-targets --features test-fixtures

# Optimised build (fat LTO, one codegen unit, abort on panic, stripped) and
# install of the three binaries into $(BINDIR). No incremental artifacts: the
# profile turns them off and `RUN` strips the env var.
release:
	$(RUN) build --release
	$(MAKE) install

# Release without the `port_forward` feature (38-tool surface), then install.
release-min:
	$(RUN) build --release --no-default-features
	$(MAKE) install

# macOS only: codesign the release binaries (ad-hoc unless CODESIGN_IDENTITY
# names a Developer ID). No-op elsewhere.
sign:
	@if [ "$(UNAME_S)" = Darwin ]; then \
		for b in $(BINARIES); do $(CODESIGN) --force --sign "$(CODESIGN_IDENTITY)" "target/release/$$b"; done; \
		echo "codesigned $(BINARIES) with $(CODESIGN_IDENTITY)"; \
	else \
		echo "codesign: skipped (non-macOS)"; \
	fi

# Install the already-built release binaries.
install:
	$(MAKE) --no-print-directory sign
	install -d "$(BINDIR)"
	for b in $(BINARIES); do install -m 0755 "target/release/$$b" "$(BINDIR)/"; done
	@echo "installed $(BINARIES) -> $(BINDIR)"

# Verify the installed binaries (and their signature on macOS).
verify:
	@for b in $(BINARIES); do test -x "$(BINDIR)/$$b" || { echo "missing $(BINDIR)/$$b"; exit 1; }; done
	@if [ "$(UNAME_S)" = Darwin ]; then \
		for b in $(BINARIES); do $(CODESIGN) --verify --strict "$(BINDIR)/$$b"; done; \
		echo "signatures verified"; \
	fi
	@echo "verified $(BINARIES) in $(BINDIR)"

# Lib tests, one process per test (nextest).
test:
	$(RUN) nextest run --test-threads $(JOBS) --lib

# Integration suite against the deterministic adapters.
test-integration:
	$(RUN) nextest run --test-threads $(JOBS) --tests --features test-fixtures

# CI schedule: retries, plus the JUnit report when nextest is built with
# the `junit` feature (NEXTEST_EXPERIMENTAL_JUNIT covers older builds).
ci-test:
	NEXTEST_EXPERIMENTAL_JUNIT=1 $(RUN) nextest run --test-threads $(JOBS) --profile ci --workspace --all-targets --features test-fixtures

fmt:
	$(RUN) fmt --all -- --check

lint:
	$(RUN) clippy --release --all-features -- -D warnings

doc:
	$(RUN) doc --no-deps --all-features

# Regenerate docs/TOOL_CATALOG.md + docs/tool-catalog.json from a live
# tools/list handshake. `tools-export-check` exits 1 when the committed
# export drifted from the binary in target/release.
tools-export:
	python3 scripts/export_tool_catalog.py

tools-export-check:
	python3 scripts/export_tool_catalog.py --check

clean:
	$(CARGO) clean
