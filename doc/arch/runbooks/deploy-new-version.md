# Runbook — deploy a new version

## Purpose

Replace the three installed binaries (`ssh-mcp`, `ssh-mcp-stdio`, `ssh-mcp-tail`) with the
release build of a selected version and confirm the installation
(`Makefile:33`, `Makefile:128-135`).

## Trigger

A version tag is available and the operator decides to move an installation to it. The
documented shape of that decision is a pull, a checkout of the tag, and a release build
(`README.md:146-158`).

## Preconditions

- A checkout of this repository at the version to deploy (`README.md:146-158`).
- A Rust toolchain at or above the MSRV 1.99 declared by the package (`Cargo.toml:16`,
  `README.md:40`).
- Write access to the install prefix. `PREFIX` defaults to `/usr/local` and the binaries land
  in `$(PREFIX)/bin`, so the build needs either `sudo` or a user prefix
  (`Makefile:24`, `Makefile:31-32`).
- On macOS, an optional signing identity. The install step signs the release binaries with
  `CODESIGN_IDENTITY`, ad-hoc when it is unset (`Makefile:26-27`, `Makefile:111-119`).

## Steps

1. Select the version in the checkout.

   ```bash
   git -C ssh-mcp pull --ff-only
   git -C ssh-mcp checkout vX.Y.Z
   ```

   Both commands are the documented upgrade and pin procedure (`README.md:146-158`).

2. Build the release binaries and install them.

   ```bash
   sudo make release
   ```

   `make release` runs the optimised build and then `install` (`Makefile:98-104`). A build
   without the `port_forward` feature replaces the same three names with the 38-tool surface:

   ```bash
   sudo make release-min
   ```

   (`Makefile:107-109`, `README.md:131`).

3. On macOS the install step codesigns before copying (`Makefile:111-126`). Pass a Developer
   ID when a real signature is required:

   ```bash
   sudo make CODESIGN_IDENTITY="Developer ID Application: ..." release
   ```

   (`Makefile:26-27`, `Makefile:111-119`).

4. Confirm the installed binaries.

   ```bash
   make verify
   ```

   `verify` fails when any of the three names is missing or not executable in `$(BINDIR)`,
   and on macOS it also runs `codesign --verify --strict` on each
   (`Makefile:128-135`).

5. Confirm that the built binary still advertises the committed tool surface.

   ```bash
   make tools-export-check
   ```

   The check regenerates the catalogue from a live `tools/list` handshake and exits non-zero
   when the committed export drifted from the binary (`Makefile:166-174`).

6. Start, reload, or replace the running service.

   The repository commits one service definition,
   `com.farchanjo.ssh-mcp.plist`: it runs `/usr/local/bin/ssh-mcp`, sets `MCP_PORT` to 45642
   and `RUST_LOG` to `info`, starts at load, keeps the process alive, and writes stdout and
   stderr to log files under `/usr/local/var/log`. The repository defines no procedure to
   load, reload, or stop it: gap. The same gap covers the other two transports, and it covers
   the container path — the `Dockerfile` builds `ssh-mcp` and `ssh-mcp-stdio` into an image
   with a non-root user, exposes port 8000, and starts `ssh-mcp`, while no build or run
   command is documented. `README.md:160` names systemd units and Docker as topics of
   `docs/OPERATIONS.md`; that document carries neither, and no systemd unit exists in the
   repository.

7. Configuration is read from the environment at resolve time; `.env.example:1-15` lists the
   connection settings and `MCP_PORT`, and the parameter, environment, default chain applies
   to each value (`docs/CONFIGURATION.md:9-25`).

## Verification

- `make verify` reports the three binaries (`Makefile:128-135`).
- `make tools-export-check` reports no drift for both catalogue files (`Makefile:166-174`).
- The release pipeline applies the same version rule on a tag: the tag must equal the
  `Cargo.toml` version, otherwise the job fails before building
  (`.github/workflows/ci.yml:172-190`).
- A first tool call from a host succeeds against the new binary. The handshake advertises
  `protocol_version = 2025-06-18` and the resource capability
  (`docs/RESOURCES.md:13-16`).

## Rollback

Reinstall the previous version by following
[rollback-previous-version.md](./rollback-previous-version.md). No on-disk state has to be
undone: sessions, lifecycles, lane snapshots and buffers live in process memory, and a
restart on another binary starts fresh (`docs/MIGRATION.md:939-941`).
