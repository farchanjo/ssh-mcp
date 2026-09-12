# Session resume — ssh-mcp v7.0.1 release + doc/test polish

**Last touched:** 2026-05-10
**Repo:** `/Users/farchanjo/dev/ssh-mcp`
**Branch:** `master` (local-only — NOT pushed)
**Latest tag:** `v7.0.1` → `9ecf939 chore(release): 7.0.1`

## What was shipped this session

### 1. v7.0.1 release (wire-additive patch on top of v7.0.0)

- `Cargo.toml` version `7.0.0 → 7.0.1`; `Cargo.lock` refreshed in lockstep.
- Headline change: `RSYNC_PROTOCOL` lifted from `31 → 32` (`src/adapters/rsync/wire/session.rs`). Wire-format identical to v31; rsync 3.4.0 incremented the protocol number as administrative signal for CVE-2024-12084..12088 + 12747 fixes (zero new wire branches). `min(local, remote)` downgrades cleanly to 31 against legacy rsync 3.2.x servers.
- Other commits-since-v7.0.0 covered by the tag:
  - `feat(mcp): env-gate v4.7 structured_content via SSH_MCP_STRUCTURED_CONTENT` (default `true`)
  - `chore(config): bump SSH_NOTIFY_DEBOUNCE_MS 200→1000 ms`, `SSH_NOTIFY_FORCE_FLUSH_MS 1→5 s`
  - `docs(mcp): mark bool args/results as JSON boolean` (preventive against `-32602 invalid type: string "true", expected a boolean` from string-quoted bool callers)
- `CHANGELOG.md` got `[7.0.1]` entry; lib tests `1966 → 1986`; clippy clean; `cargo build --release --all-features` OK.
- Tag `v7.0.1` ancorada em `9ecf939`. Doc polish commits que vieram depois NÃO foram dobrados na tag (deliberadamente — opção 3 escolhida pelo user; doc fixes vivem em master como polish pós-release).

### 2. Doc inconsistency sweep (28 MD files audited)

Methodology:
- 28 haiku dispatches (one per MD file) → extract every factual claim into flat YAML.
- 3 sonnet:medium dispatches (top-level / docs/ / docs/adr) → cross-validate haiku findings vs ground truth + flag stuff haiku missed.
- 1 sonnet:medium fix-pass → apply 35 edits across 11 files.

Result: 11 file-level commits (`5672919` and earlier sequence — see `git log --oneline -20`). Working tree clean.

Critical drift fixed in living docs:
- `CLAUDE.md` (project root, loaded into every session): tool counts `36/35 → 39/38`, push schemes `6 → 7`, error codes `40 → 46`, `RSYNC_PROTOCOL` prose `v31 → v32 (downgrades to v31)`, lib tests `1966 → 1986`, "six layered deltas" → "seven", header version `v7.0.0 → v7.0.1`.
- `README.md`: rsync proto prose v31 → v32; lib tests 1966 → 1986.
- `docs/ARCHITECTURE.md`: title `v7.0.0 → v7.0.1`; wire-proto prose v31 → v32; Mermaid handshake nodes `proto 27 → 31` → `proto 27 → 32` (push + pull pipelines); capabilities table `6 schemes → 7 schemes`.
- `docs/adr/0011-rsync-hybrid-transport.md`: Status section + Test Coverage table updated. ALL other v31 mentions in ADRs are time-capsule plan narrative — left untouched.
- `docs/MIGRATION.md`: only L869 (current-state v6.1→v7.0 paragraph) updated. ALL slice narratives under `### v7.0.0-alpha.X` headers preserved as historical record.
- Plus refreshes in CI.md, REVIEW_CHECKLIST.md, CONTRIBUTING.md (added `tests/lockfree_invariants_rsync.rs` reference), DAEMON.md, DEVELOPMENT.md, LLM_GUIDE.md.

### 3. Binary install + codesign

- `cargo build --release --all-features` OK.
- `sudo cp target/release/{ssh-mcp,ssh-mcp-stdio,ssh-mcp-tail} /usr/local/bin/`
- `sudo codesign --sign - --force --options runtime` — ad-hoc sign with hardened runtime on each binary.
- `codesign --verify --verbose=4` — clean across all 3.
- Mach-O thin arm64. `Format=Mach-O thin (arm64)`, `Signature=adhoc`, owner `root:wheel`.

### 4. Smoke test (target/release binaries)

```
ssh-mcp        : exit=124 (HTTP server up — addr=0.0.0.0:8000, peer GC spawned)
ssh-mcp-stdio  : exit=1   (stdio transport up, ConnectionClosed esperado — sem cliente MCP)
ssh-mcp-tail   : exit=0   — version 7.0.1
```

`ssh-mcp` and `ssh-mcp-stdio` lack `--version` / `--help` flags (they jump straight into runtime). Optional follow-up: add `clap` `Parser` to those two for deploy validation.

### 5. Python integration test polish (3 commits)

Validated by sonnet:medium against v7.0.1 surface. 7 files touched, 3 semantic commits:

- `5672919 fix(tests): tool-list count assertion {20,21} → {38,39} for v7.0.1` — hard-fail fix in `test_http.py` + `test_stdio.py`. Function renamed `*_v47_catalogue → *_v7_catalogue`.
- `0ef02a0 docs(tests): refresh stale tool/error counts in v47/v5 docstrings` — module docstrings only (`test_v47_structured_content.py`, `test_v5_adversarial.py`). Bodies clean.
- `ebeb6ba docs(tests/v7-rsync): refresh slice-N narrative + xfail tags for v7.0.1` — `test_v7_rsync_vm.py`, `test_v7_rsync_http.py`, `helpers/rsync_client.py`. Inverted comment at `test_v7_rsync_vm.py:385-386` corrected. xfail reasons bumped `v7.0.0-alpha.8 → v7.0.1`. Defensive `_wait_terminal_or_files` helper kept; commentary clarifies `status="completed"` is the v7.0.1 happy path.

`pytest --collect-only` clean. `python -m py_compile` on all 7 modified files OK.

## What is OPEN / pending

### Immediate next step (where session pause happened)

**Run pytest against v7.0.1**. Plan was 2-haiku parallel dispatch:
- **FG haiku**: `pytest scripts/test_v7_rsync_vm.py -v --tb=short` (only file touching real `vm.services` rsync 3.2.7).
- **BG haiku**: `pytest scripts/ --ignore=scripts/test_v7_rsync_vm.py -v --tb=line` (paramiko-fixture suite, ~22 files).

Brief should set `cd /Users/farchanjo/dev/ssh-mcp && source .venv/bin/activate` first. Default env vars match the VM (`SSH_MCP_E2E_HOST=vm.services` / `_USER=root` / `_PORT=22` / `_KEY_PATH=~/.ssh/id_rsa`). Bash timeout ≥ 600000ms (some tests run for minutes against paramiko fixtures).

Expected outcome:
- 19 passed + 2 xfailed for v7 rsync trio (per CLAUDE.md baseline) — the 2 xfails cover the deferred local-FS adapter for `RsyncSftpFsPort`. xfails strict=False so they show as `xfailed`, not `failed`.
- `test_v7_rsync_vm.py` against rsync 3.2.7: should NOT surface `RSYNC_PROTOCOL_ERROR` for the wire path. Pre-cleanup tests had defensive elif accepting that error; v7.0.1 should hit the happy path. Defensive elif kept — if test goes through it, that's a regression worth investigating.

vm.services already verified reachable: `ssh -l root vm.services 'rsync --version'` → `rsync 3.2.7 protocol version 31`.

### Other open threads

- **`git push`**. Master + `v7.0.1` tag are local-only. Run `git push origin master v7.0.1` when ready.
- **GitHub release notes**. `gh release create v7.0.1 --notes-file <(awk '/^## \[7.0.1\]/,/^## \[7.0.0\]/' CHANGELOG.md | sed '$d')`.
- **Statusline caveman badge** (cosmetic, optional). Add to `~/.claude/settings.json`:
  ```json
  "statusLine": {
    "type": "command",
    "command": "bash \"/Users/farchanjo/.claude/plugins/cache/caveman/caveman/84cc3c14fa1e/hooks/caveman-statusline.sh\""
  }
  ```
- **Optional follow-up**: add `clap` `--version` / `--help` flag handlers to `ssh-mcp` and `ssh-mcp-stdio` binaries so deploy validation has a clean `exit 0` path.

## Ground-truth table (for any agent dispatched in next session)

| key | value | source |
|---|---|---|
| version | 7.0.1 | Cargo.toml + git tag v7.0.1 |
| lib_tests_passed | 1986 | `cargo test --lib --quiet` |
| integration_tests | 134 across 9 binaries | tests/ |
| loom_invariants | 27 (20 + 7 across 2 files) | tests/lockfree_invariants*.rs |
| rsync_proto_advertised | 32 (downgrades to 31 against legacy via min(local,remote)) | src/adapters/rsync/wire/session.rs `RSYNC_PROTOCOL = 32` |
| rsync_proto_min/max | 27 / 32 | RSYNC_PROTOCOL_MIN/MAX |
| error_codes_total | 46 (38 base + 2 v6.1 + 6 v7.0) | ADR 0007 + 0010 + 0011 |
| msrv | 1.95 | Cargo.toml |
| edition | 2024 | Cargo.toml |
| ADRs | 11 (0001-0011) | docs/adr/ |
| tools_with_port_forward | 39 | #[tool] count in src/infra/mcp/tool_router.rs |
| tools_without_port_forward | 38 |  |
| ssh_tools | 24 | `ssh_*` |
| sub_tools | 9 | `sub_*` |
| serial_tools | 6 | `serial_*` |
| push_schemes | 7 (shell, command, transfer, session, forward, serial, rsync) |  |
| notify_debounce_default | 1000 ms | SSH_NOTIFY_DEBOUNCE_MS |
| notify_force_flush_default | 5000 ms | SSH_NOTIFY_FORCE_FLUSH_MS |
| MCP protocolVersion | 2025-06-18 |  |
| structured_content | env-gated by `SSH_MCP_STRUCTURED_CONTENT` (default `true`) |  |
| `release_when_no_subs` | bool arg — JSON `true`/`false` only, NEVER string `"true"` | rmcp/serde rejects with `-32602 invalid type: string "true", expected a boolean` |
| vm.services | rsync 3.2.7 / protocol 31 — root@vm.services:22 with `~/.ssh/id_rsa` |  |

## Convention reminders

- **Caveman mode**: full intensity active. Drop articles/filler/pleasantries. Fragments OK. Code/commits/security: full English.
- **Haiku-fit gate** (per CLAUDE.md): single-file, zero judgment, fixed output shape, ≤200-word brief, bounded acceptance. Multi-file work goes to `sonnet:medium`.
- **Dispatch labeling** (mandatory): `[haiku] <task>` (no effort tier) | `[sonnet:<low|medium|high|xhigh|max>] <task>` | `[opus:<effort>] <task>` (Opus FORBIDDEN unless `/opus` literal in current user prompt).
- **Concurrency guard**: cap=20 per turn. Always 1 FG + N BG. Pair every BG dispatch with notification awaiting; do NOT poll.
- **Commit hygiene**: NEVER commit unless user asks. NEVER mass-commit — break into small contextual commits. Commits use Conventional Commits format (`type(scope): subject`). Subject ≤ 72 chars (project hook enforces).
- **Tests + lint**: `cargo clippy --release --all-features -- -D warnings` + `cargo test --lib --quiet` must stay green after any code change.
- **PMD config**: NEVER touch without explicit user permission (note: this repo is Rust, no PMD; rule lives in global CLAUDE.md as cross-project guard).
- **CLAUDE.md global lock**: `/Users/farchanjo/.claude/CLAUDE.md` requires literal `/global-md` in current prompt to edit. Single-shot, 6h window.

## Quick-resume checklist

When resuming:
1. `cd /Users/farchanjo/dev/ssh-mcp`
2. `git status -s` — must be clean (working tree should be empty after this session).
3. `git log --oneline -5` — verify top is `ebeb6ba docs(tests/v7-rsync): refresh slice-N narrative + xfail tags for v7.0.1`.
4. Decide: run pytest now? push? GitHub release? something else?
5. If running pytest: dispatch 2 parallel haikus per the plan above. Aggregate, report PASS/FAIL counts to user.
