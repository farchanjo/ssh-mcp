# Dev entry points — the single path for building and testing this repo.
#
# Everything runs through here so the knobs stay in one place:
#   * JOBS is pinned to 4 for every cargo invocation, dev and release alike.
#     It is exported as CARGO_BUILD_JOBS and passed to nextest as
#     --test-threads; `.cargo/config.toml` sets the same 4 for a bare cargo
#     call. An unset JOBS would mean "all cores" — pinning keeps wall-clock
#     predictable and leaves headroom for the rest of the machine. A value
#     above the core count is clamped to it; a non-integer is an error.
#     Override per run: `JOBS=8 make test`.
#   * Dev and test are the default path: incremental, opt-level 0, sccache,
#     4 jobs. nextest is the only test runner (one process per test).
#   * Release is the owner's path: every codegen optimisation, no incremental
#     artifacts. Agents never run it — see AGENTS.md → Working agreement.
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
#   * `make all` is the owner's whole pipeline: gates + release + install +
#     verify. Sessions stop at `make gates`.
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
# Deliberately not $(CORES): the repo pins 4 everywhere so timings are stable
# and the machine stays usable during a build. Raise it per run if needed.
JOBS ?= 4
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

.PHONY: info dev check gates all release release-min sign install verify test test-integration ci-test fmt lint lint-dev doc tools-export tools-export-check clean

# Show the effective knobs before a long run.
info:
	@echo "jobs=$(JOBS) (pinned; $(CORES) cores available) cargo=$(CARGO) bindir=$(BINDIR) identity=$(CODESIGN_IDENTITY) wrapper=$(RUSTC_WRAPPER) native=$(NATIVE) incremental=dev/test-on release-off runner=nextest"

# Full owner pipeline: gates, release build, codesign, install, verify.
# Agents stop at `make gates` — the release half belongs to the owner.
all:
	$(MAKE) --no-print-directory fmt
	$(MAKE) --no-print-directory lint
	$(MAKE) --no-print-directory test
	$(MAKE) --no-print-directory release
	$(MAKE) --no-print-directory verify

# Incremental, unoptimised build — the default loop (sccache + 4 jobs).
dev:
	$(RUN) build

check:
	$(RUN) check --all-targets --features test-fixtures

# Dev-only pipeline: format, dev-profile clippy, both nextest suites. No
# release build, no install — this is the agent and contributor path.
gates:
	$(MAKE) --no-print-directory fmt
	$(MAKE) --no-print-directory lint-dev
	$(MAKE) --no-print-directory test
	$(MAKE) --no-print-directory test-integration

# Owner-only. Optimised build (fat LTO, one codegen unit, abort on panic,
# stripped) and install of the three binaries into $(BINDIR). No incremental
# artifacts: the profile turns them off and `RUN` strips the env var.
# sccache and the pinned 4 jobs still apply, exactly as in dev.
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

# Canonical clippy gate, owned by CI. It compiles for the release profile, so
# a cold target dir makes it the slowest step in the loop.
lint:
	$(RUN) clippy --release --all-features -- -D warnings

# The same lint set compiled for the dev profile — the fast local variant.
# Use this while iterating; CI still runs `lint`.
lint-dev:
	$(RUN) clippy --all-features -- -D warnings

doc:
	$(RUN) doc --no-deps --all-features

# Regenerate docs/TOOL_CATALOG.md + docs/tool-catalog.json from a live
# tools/list handshake. `tools-export-check` exits 1 when the committed
# export drifted from the binary in target/debug (the script follows
# SSH_MCP_BUILD_PROFILE and per-binary SSH_MCP_*_BIN overrides).
tools-export:
	python3 scripts/export_tool_catalog.py

tools-export-check:
	python3 scripts/export_tool_catalog.py --check

clean:
	$(CARGO) clean
