# Quality Scenarios

These scenarios state the quality the ssh-mcp server holds itself to, in the
ISO/IEC 25010:2023 characteristic set. Every row names the source and the
stimulus, the environment it is exercised in, the response the repository
implements, and the response measure. A measure carries a number only when
the repository states that number: a configured default, a cap or a timeout,
always with the file and line that states it. A scenario whose proof is a
test, a lint or a gate names that proof directly; one the repository cannot
prove is marked unproven.

Sources for the scenarios are the project constitution (`P-01` to `P-09`,
`doc/arch/memory/constitution.md:89-124`), the ADRs under `docs/adr/`,
`CI.md`, `REVIEW_CHECKLIST.md`, `docs/DEVELOPMENT.md` and the test suites
under `tests/`.

| ID | Attribute | Stimulus | Environment | Response | Measure |
| --- | --- | --- | --- | --- | --- |
| QS-001 | functional-suitability | Source: an MCP host. It completes `initialize` and calls `tools/list`. | Default features, `port_forward` on (`docs/DEVELOPMENT.md:122`). | The server advertises its capability set and the whole tool surface. | The surface counts 39 tools with `port_forward` (`docs/DEVELOPMENT.md:122`); proven by `tests/v4_smoke.rs::prod_server_exposes_full_tool_surface` (`tests/v4_smoke.rs:50`), which asserts each of the 17 core tool names (`tests/v4_smoke.rs:15-33`) plus `ssh_forward` (`:61-70`) is present with a non-empty description, and by `tests/v4_smoke.rs::prod_build_server_advertises_capabilities` (`:36`), which asserts the `tools` and `resources` capabilities. |
| QS-002 | functional-suitability | Source: a caller. It runs `ssh_rsync` with `transport=auto` against a host whose rsync is missing. | Session established; the remote probe already ran. | The server selects the SFTP lane and syncs instead of failing. | Selected transport is `sftp`; proven by `tests/v7_rsync_smoke.rs::transport_auto_routes_to_sftp_when_rsync_missing` (`tests/v7_rsync_smoke.rs:201`), with the wire leg by `tests/v7_rsync_smoke.rs::transport_auto_routes_to_wire_when_rsync_v31_present` (`tests/v7_rsync_smoke.rs:184`). |
| QS-003 | functional-suitability | Source: a caller. It retries an interrupted `ssh_upload` with `resume=true`. | The destination holds a partial file of n bytes. | The transfer continues from byte n and reports the offset it resumed from. | `resumed_from` equals the destination size; proven by `tests/v6_resume_smoke.rs::upload_resume_partial_destination_carries_offset` (`tests/v6_resume_smoke.rs:309`) and `tests/v6_resume_smoke.rs::download_resume_partial_destination_carries_offset` (`tests/v6_resume_smoke.rs:449`). |
| QS-004 | performance-efficiency | Source: a stalled MCP peer. It stops reading while the producer keeps emitting. | One lane driven through `SubscriberLaneAdapter::produce` (`src/adapters/subscription/subscriber_lane.rs:424`) — a component production does not route deliveries through. | Drops are counted, the producer keeps running, no task panics. | Lane capacity 1024 by default (`src/adapters/config/internal/mod.rs:186`); proven for the lane adapter in isolation by `tests/chaos/chaos04_slow_consumer_overflow.rs::chaos04_slow_consumer_drop_oldest_keeps_producer_running`; the production delivery path does not pass through `produce`. |
| QS-005 | performance-efficiency | Source: N subscribers. N lanes are registered on the channel mux at the same time. | Channel mux round-robin, drain task spawned by the test (`ChannelMuxAdapter::spawn_drain`, `src/adapters/subscription/channel_mux.rs:120`) — production never spawns it. | Every lane receives a turn and no lane starves. | No numeric fairness bound is stated in the repository; proven for the mux in isolation by `tests/chaos/chaos23_channel_mux_fairness_n_lanes.rs::chaos23_channel_mux_fairness_n_lanes_no_starvation` and by the loom invariant `loom_mux_round_robin_no_starvation` (`docs/DEVELOPMENT.md:109`); no drain task runs in production. |
| QS-006 | performance-efficiency | Source: the MCP host. It calls `sub_stats` while writers push on the same lane. | Concurrent producers, lock-free counters. | The call returns a consistent snapshot without blocking a writer. | No latency budget is stated in the repository; proven by `tests/chaos/chaos20_sub_stats_read_during_active_writes.rs::chaos20_stats_read_during_writes_is_monotonic`. |
| QS-007 | performance-efficiency | Source: a consumer. It does not drain a full lane under the `block_slow` policy. | The sync `produce` route (`src/adapters/subscription/subscriber_lane.rs:424`) with `SSH_BP_BLOCK_TIMEOUT_MS` default 5000 (`src/adapters/config/internal/mod.rs:210`) — the route production does not use. | The block gives up and the call falls back to snapshot semantics instead of hanging. | 5000 ms; proven for the lane adapter in isolation by `tests/chaos/chaos21_lag_policy_block_slow_timeout_fallback.rs::chaos21_block_slow_falls_back_to_snapshot_on_full`, which drives `produce`; `lane_dispatch_block_slow` itself degrades to `lane_dispatch_snapshot` (`subscriber_lane.rs:676-686`). |
| QS-008 | compatibility | Source: an MCP host. It reads a tool response today. | Current binary, default features. | The block-markdown envelope keeps its shape — first line `TOOL_NAME: STATUS`, one `KEY: value` per line, output blocks headed `--- name [nonce] ---` — and the body of a tool is pinned as exact text. | Proven for the current render by exact-string assertions in the render test modules: `src/infra/mcp/helpers/output.rs::tests::render_output_block_with_content` (`:244`) pins `--- stdout [deadbeef] ---\nhi` and `::render_output_block_empty_buffer` (`:238`) pins the empty form with its nonce; `src/infra/mcp/render/connection.rs::tests::disconnect_render_emits_block` (`:881`) pins the whole body `SSH_DISCONNECT: OK\nSESSION_ID: sess-abc` and `::disconnect_agent_renders_counts` (`:905`) pins `SSH_DISCONNECT_AGENT: OK\nAGENT_ID: alpha\nSESSIONS: 3\nCOMMANDS: 5`; `src/infra/mcp/render/rsync.rs::tests::cancel_render_pins_status_line` (`:313`) pins `SSH_RSYNC_CANCEL: OK\nRSYNC_ID: rs-9`; `src/infra/mcp/helpers/structured.rs::tests::ok_payload_carries_text_and_structured` (`:118`) pins that the twin carries the JSON value and the text body passes through verbatim. These pin the current render of a tool; no test compares a response against a captured earlier-release body, so the "byte-identical with a v3 or v4 capture" half is unproven. |
| QS-009 | compatibility | Source: a v7.0.x consumer. It runs the daemon without opting into inline push. | `SSH_INLINE_PUSH_DAEMON_RELAY` default false (`src/adapters/config/internal/mod.rs:1002`). | The stream carries no `inline_push` event and `sub_stats` omits the inline counter lines. | Proven by `src/infra/mcp/render/subscription.rs::tests::renders_no_inline_lines_when_inline_push_false` (`src/infra/mcp/render/subscription.rs:992`) and `::omits_inline_counters_when_inline_disabled` (`:1025`). |
| QS-010 | compatibility | Source: a distributor. It builds the package without optional features. | `--no-default-features`. | The binary compiles and advertises 38 tools instead of 39. | 38 tools (`CI.md:29`); gate `cargo build --no-default-features` (`CI.md:29`), feature table at `docs/DEVELOPMENT.md:122`. |
| QS-011 | interaction-capability | Source: a model. It reads a successful tool response. | Any tool on the wire. | The response carries a `NEXT:` successor line and the hygiene block so the next call is implied. | Proven by `src/infra/mcp/render/subscription.rs::tests::next_line_ordering_pushes_subscribe_first_in_subscribe_render` (`src/infra/mcp/render/subscription.rs:894`) and by the `sub_open uri=session://` assertion in `src/infra/mcp/render/connection.rs::tests` (`src/infra/mcp/render/connection.rs:902`). |
| QS-012 | interaction-capability | Source: the leak watcher. A resource stays `Owned` past the warn threshold with zero subscribers. | The model then calls a list-style tool. | The next list response carries a `WARN: SUB_LEAK_RISK` line and a matching `warnings` entry naming the resource, its age and severity. | Warn threshold 2 s by default (`src/adapters/config/internal/mod.rs:290`); proven by `src/infra/mcp/render/connection.rs::tests::list_sessions_render_appends_warn_line_for_each_alert` (`src/infra/mcp/render/connection.rs:1109`) and `tests/chaos/chaos31_sub_leak_risk_warn_vs_sub_arrives_in_window.rs::chaos31_warn_vs_subscribe_inside_window_resolves_deterministically`. |
| QS-013 | interaction-capability | Source: a model. It names an id that does not exist. | A not-found error path. | The error names the closest ids so the retry repairs itself. | Proven by `src/infra/mcp/suggestions.rs::tests::closest_ids_finds_neighbors` (`src/infra/mcp/suggestions.rs:137`) and `::closest_ids_caps_at_n` (`:170`). |
| QS-014 | reliability | Source: concurrent use-case calls. Two operations act on one resource lifecycle at the same time. | Lifecycle CAS state machine. | Exactly one terminal outcome is produced; no invalid transition and no double close occurs. | Proven by `tests/property/state_machine.rs::prop_state_machine_no_invalid_transitions` (`tests/property/state_machine.rs:56`), `tests/property/state_machine.rs::prop_no_double_close` (`:94`) and `tests/chaos/chaos27_grace_fire_vs_explicit_close_race.rs::chaos27_grace_vs_close_closes_exactly_once`. |
| QS-015 | reliability | Source: a burst producer. It writes through `SubscriberLaneAdapter::produce` (`src/adapters/subscription/subscriber_lane.rs:424`) into a lane whose mpsc is full, once per lag policy. | Drop-oldest, drop-newest, snapshot and block-slow policies, on the route production does not use. | The call returns an outcome and never panics. | Proven for the lane adapter in isolation by `tests/chaos/chaos11_mpsc_full_panic_safety.rs::chaos11_drop_oldest_never_panics_on_full`, `::chaos11_drop_newest_never_panics_on_full`, `::chaos11_snapshot_never_panics_on_full` and `::chaos11_block_slow_does_not_panic_when_consumer_drains_async`; `produce` has test-only callers. |
| QS-016 | reliability | Source: concurrent callers. They advance and read the lane byte cursor. | `SubscriberLanePort::advance_cursor` / `current_cursor` (`src/ports/subscriber_lane.rs:94-98`), whose only production mutation site is elsewhere: `lane_bridge.rs:189` advances the same atomic for the inline-push leg and `subscriber_lane.rs:868` reads it for `sub_replay`. | The cursor is monotonic and converges on the highest written position. | Proven by `tests/property/cursor.rs::prop_cursor_monotonic` (`tests/property/cursor.rs:38`, driving `advance_cursor` at `:56`) and `tests/property/cursor.rs::prop_cursor_converges_on_max` (`:67`, `:84`); those port methods have no production caller. The third leg, `tests/chaos/chaos18_replay_during_concurrent_producer.rs::chaos18_replay_during_producer_keeps_cursor_monotonic`, drives `produce` (test-only) and is proven for the lane adapter in isolation. |
| QS-017 | reliability | Source: the model checker. Interleaved concurrent access runs against the lock-free state carriers. | `RUSTFLAGS="--cfg loom"`, scoped harnesses. | Every invariant holds under the explored interleavings. | 32 invariants across three files (`docs/DEVELOPMENT.md:114`, `CI.md:69`); harness `tests/lockfree_invariants.rs`, `tests/lockfree_invariants_rsync.rs`, `tests/lockfree_invariants_inline_push.rs`. Full-binary model checking is unavailable because of an upstream tokio/loom conflict (`CI.md:77`). |
| QS-018 | security | Source: an impostor or a rebuilt host. The presented host key differs from the recorded one. | `known_hosts` holds a key for that host. | The connection is rejected and the mismatch is logged with host and port. | Proven by `src/adapters/ssh/internal/session.rs::tests::recorded_key_mismatch_is_rejected` (`src/adapters/ssh/internal/session.rs:167`); the learn-on-first-use path by `::unknown_host_is_learned_via_tofu` (`:190`). |
| QS-019 | security | Source: an operator. It turns host-key verification off on purpose. | `SSH_MCP_INSECURE_SKIP_HOST_KEY_CHECK` true. | Verification is skipped, and the setting is documented as insecure. | Documented INSECURE at `src/adapters/config/internal/mod.rs:22`; proven by `src/adapters/ssh/internal/session.rs::tests::skip_flag_accepts_any_host_key` (`src/adapters/ssh/internal/session.rs:154`). |
| QS-020 | security | Source: a contributor. It stages a credential, a private key or an internal hostname in the corpus. | The validation gate runs before every commit. | Validation fails on the secret-scan family and the tree stays free of credential assignments. | Gate: `speckit validate`, rule `secret-scan`; the reviewer box at `REVIEW_CHECKLIST.md:57`. |
| QS-021 | maintainability | Source: a contributor. A change is proposed for the dev loop. | Dev profile, nextest as the only runner. | `make gates` passes end to end: format check, dev-profile clippy with `-D warnings`, the lib suite and the integration suite. | Gate `make gates` (`Makefile:92-96`), mandate `P-06` (`doc/arch/memory/constitution.md:109-112`). |
| QS-022 | maintainability | Source: a contributor. Production code calls `unwrap`, `expect` or `panic!`. | Compile and lint. | The lint gate fails the change. | `forbid`-level lints at `Cargo.toml:158-168`; clippy gate at `Makefile:155-156`; policy at `CI.md:13`. |
| QS-023 | maintainability | Source: a reviewer. A change crosses a layer boundary or puts a `Mutex` on the hot path. | Review of the pull request. | The reviewer box is either ticked with the code as written or the merge is blocked. | `REVIEW_CHECKLIST.md:19-20` (layer direction, no hot-path lock) and `REVIEW_CHECKLIST.md:70` (a box a reviewer cannot tick blocks the merge). |
| QS-024 | flexibility | Source: a maintainer. A new capability is added to the published wire. | Existing hosts keep running against the new binary. | The addition is opt-in and additive; a host that does not opt in sees the wire it saw before. | Mandate `P-05` (`doc/arch/memory/constitution.md:105-107`); the `release_when_no_subs` opt-in default at `REVIEW_CHECKLIST.md:37`; proof instance in QS-009. |
| QS-025 | flexibility | Source: a distributor. It builds both feature extremes of the package. | `--all-features` and `--no-default-features`. | Both builds compile clean. | Gates at `CI.md:11` and `CI.md:29`. |
| QS-026 | safety | Source: a caller. It runs `ssh_rsync` without asking for deletion. | Push direction against a populated destination. | No destination path is removed. | `delete` defaults to false (`src/infra/mcp/args/rsync.rs:125-126`); the response omits the line when unset, proven by `src/infra/mcp/render/rsync.rs::tests::started_render_omits_dry_run_and_delete_when_unset` (`src/infra/mcp/render/rsync.rs:244`); the deletion path itself by `src/adapters/rsync/sftp/executor.rs::tests::delete_removes_file_on_destination` (`src/adapters/rsync/sftp/executor.rs:781`). |
| QS-027 | safety | Source: a caller. It previews a destructive sync with `dry_run=true`. | Deletes and overwrites planned on the destination. | Every destructive action is skipped and reported as a skip. | Proven by `src/adapters/rsync/sftp/executor.rs::tests::dry_run_skips_every_destructive_action` (`src/adapters/rsync/sftp/executor.rs:680`); contract stated at `src/adapters/rsync/sftp/executor.rs:14`. |

## Proof status

- Proven: every scenario above names its proof. Where the proof is a test
  that the PR gate runs (`CI.md:9-15`, `CI.md:22-37`), the scenario is
  verified on every pull request. Where the proof is a lint, a `make` target
  or a reviewer box, the scenario is verified by that gate rather than by a
  test.
- Proven only for an unwired component — QS-004, QS-005, QS-007, QS-015 and
  the `chaos18` leg of QS-016: each of those tests drives code that
  production does not route deliveries through. `SubscriberLaneAdapter::produce`
  (`src/adapters/subscription/subscriber_lane.rs:424`) is the only caller of
  `lane_dispatch` and of the four policy functions (`subscriber_lane.rs:429`).
  `findReferences` at its definition returns 29 references across 14 files and
  `incomingCalls` lists 25 caller functions, every one of them a test: the
  file's own test module (`subscriber_lane.rs:1201`, `:1229`, `:1253`, `:1274`,
  `:1326`, `:1356`), `tests/property/lag_policy.rs:58`,
  `tests/property/lane_mpsc.rs:57`, `tests/chaos/*` and
  `tests/v5_smoke.rs:151`. `ChannelMuxAdapter::spawn_drain`
  (`src/adapters/subscription/channel_mux.rs:120`) is called only in its own
  test module (`:278`, `:325`, `:362`); `findReferences` and `incomingCalls`
  answer empty at its definition because of its `self: &Arc<Self>` receiver,
  so that list comes from a text search over `src/` and `tests/`. `src/composition/prod.rs:344` binds the mux outbound receiver
  to `_mux_outbound_rx`, a local of `build_use_cases` that is dropped when it
  returns, so no drain task runs in production. What production uses instead
  is `LaneFanoutBridge::notify_one_lane`
  (`src/adapters/subscription/lane_bridge.rs:280-289`), reached through
  `notify_lanes` (`:494-512`) from `MemoryRegistry::broadcast`
  (`src/adapters/subscription/memory_registry.rs:975-977`): it calls the
  notifier directly, with no lag policy, no lane queue and no read of the
  lane `paused` flag (`paused` appears nowhere in `lane_bridge.rs`).
- Unproven in part: QS-008 proven for the current render only — the
  "byte-identical with a v3 or v4 capture" half has no test. QS-017 proves
  the scoped invariants and does not prove the whole binary, because full
  loom mode is blocked by an upstream tokio/loom conflict (`CI.md:77`).
  QS-023 is a review box and no automated check enforces it.
- Drift found while writing this document: `REVIEW_CHECKLIST.md:52` and
  `CONTRIBUTING.md:62` state that snapshot tests in `tests/v4_smoke.rs` pin
  the legacy wire, and no such test exists there. The file asserts the server
  name, the `tools` and `resources` capabilities, the presence of each core
  tool name with a non-empty description, and the `port_forward` presence of
  `ssh_forward` (`tests/v4_smoke.rs:36-71`); it asserts no response body. Its
  header comment claims "18 with `port_forward`, 17 without"
  (`tests/v4_smoke.rs:4-5`) against the 38 / 39 tools of `CI.md:29`.
- Not claimed: no scenario in this document asserts a throughput, a
  latency budget or an availability percentage. The repository states no
  such number, and no benchmark target exists: `Cargo.toml:25-57` declares
  three binaries, one library and one test target, and no `[[bench]]`. The
  stress scripts at `CI.md:85-91` run outside the PR gate and hold no
  threshold.
