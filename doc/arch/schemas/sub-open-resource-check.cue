// DDD role: ValueObject

package schemas

// #Check is FR-001 to FR-003: the kind policy, where it runs, and the answer
// each refused kind produces. Every line was read from the file.
// DDD role: ValueObject
#Check: {
	call_site:    "src/application/subscription_admin.rs:135-162"
	before_open:  "src/application/subscription_admin.rs:144-147"
	parse_first:  "src/application/read_resource.rs:164-192"
	checked: {
		shell: {
			probe:   "src/application/subscribe_resource.rs:192-201"
			variant: "DomainError::ShellNotFound"
			code:    "SHELL_NOT_FOUND"
			reason:  "no active shell with the given ID"
			detail:  "Recreate via ssh_shell_open; subscribe to shell://<id>/output to track lifecycle."
			source:  "src/infra/mcp/tool_router.rs:793-797; src/infra/mcp/error_detail.rs:84-86"
		}
		command: {
			probe:   "src/application/subscribe_resource.rs:203-212"
			variant: "DomainError::CommandNotFound"
			code:    "COMMAND_NOT_FOUND"
			reason:  "no async command with the given ID"
			detail:  "Use ssh_commands; subscribe to command://<id>/output for completion events."
			source:  "src/infra/mcp/tool_router.rs:798-802; src/infra/mcp/error_detail.rs:87-89"
		}
		transfer: {
			probe:   "src/application/subscribe_resource.rs:214-223"
			variant: "DomainError::TransferNotFound"
			code:    "TRANSFER_NOT_FOUND"
			reason:  "no transfer with the given ID"
			detail:  "Re-issue ssh_upload or ssh_download to obtain a fresh transfer_id."
			source:  "src/infra/mcp/tool_router.rs:803-807; src/infra/mcp/error_detail.rs:90-92"
		}
		session: {
			probe:   "src/application/subscribe_resource.rs:225-234"
			variant: "DomainError::SessionNotFound"
			code:    "SESSION_NOT_FOUND"
			reason:  "no active SSH session with the given ID"
			detail:  "Use ssh_sessions; recreate via ssh_connect if the id is stale."
			source:  "src/infra/mcp/tool_router.rs:788-792; src/infra/mcp/error_detail.rs:83"
		}
	}
	permissive: {
		forward: {reason: "a forward subscriber may attach before the producer publishes", source: "src/application/subscribe_resource.rs:175-177"}
		serial:  {reason: "serial state lives outside the application repositories; serial_open validates first", source: "src/application/subscribe_resource.rs:178-182"}
		rsync:   {reason: "rsync sessions live outside the application repositories; ssh_rsync mints the id first", source: "src/application/subscribe_resource.rs:183-187"}
	}
	answer: {
		renderer:      "src/infra/mcp/tool_router.rs:167-173"
		error_arm:     "src/infra/mcp/tool_router.rs:1320"
		body:          "src/infra/mcp/helpers/error.rs:31-53"
		structured:    "src/infra/mcp/helpers/error.rs:74-99"
		detail_merge:  "src/infra/mcp/error_detail.rs:291-299"
		first_line:    "SUB_OPEN: ERROR"
		detail_suffix: "; <id>"
	}
	frozen_description: {
		claims: ["RESOURCE_NOT_FOUND"]
		lines:  ["src/infra/mcp/tool_router.rs:2389", "docs/TOOL_CATALOG.md:1475", "docs/tool-catalog.json:1501"]
		edit:   false
		check:  "make tools-export-check"
	}
}

// #Production is FR-011: the composition installs the guard, so the
// hand-built tests cannot stay green while production accepts a missing id.
// DDD role: ValueObject
#Production: {
	field:        "src/composition/mod.rs:192-194"
	build_site:   "src/composition/prod.rs:544-547"
	builder:      "src/application/subscription_admin.rs:106"
	proving_test: "tests/sub_open_resource_check.rs::production_sub_subscribe_refuses_missing_resources_and_opens_forward"
	why:          "FR-001 and FR-002 build the use case by hand; without this requirement composition can forget the guard"
}

// #Policy is FR-004: the one implementation and its constraints.
// DDD role: ValueObject
#Policy: {
	evidence: {
		existing_implementation: "src/application/subscribe_resource.rs:156-190"
		existing_tests: [
			"src/application/subscribe_resource.rs:478",
			"src/application/subscribe_resource.rs:506",
			"src/application/subscribe_resource.rs:545",
		]
		constraints: [
			"no adapter imports crate::application",
			"the kind table exists once in src/",
			"SubscribeUseCase stays dyn-safe over Arc<dyn LaneAdmin>",
			"resources/subscribe keeps its five ensure tests green",
		]
	}
	risk_surface: {
		subscribe_new:      "src/application/subscription_admin.rs:93"
		subscribe_execute:  "src/application/subscription_admin.rs:135"
		subscribe_builder:  "src/application/subscription_admin.rs:106"
		production_wiring:  "src/composition/prod.rs:544-547"
		new_references:     26
		execute_references: 31
	}
}

// #NoLane is FR-002: a refusal costs nothing.
// DDD role: ValueObject
#NoLane: {
	order: {
		sequence: "check, then LaneAdmin::open"
		open:     "src/application/subscription_admin.rs:144-147"
		reserve:  ["src/adapters/subscription/subscriber_lane.rs:811", "src/adapters/subscription/subscriber_lane.rs:509-540"]
	}
	cost: {
		debouncer: ["src/application/subscription_admin.rs:148-150", "src/adapters/subscription/memory_registry.rs:371-410"]
		caps:      ["src/adapters/config/internal/mod.rs:200", "src/adapters/config/internal/mod.rs:205"]
	}
}

// #Documentation is FR-006, FR-007 and FR-008: the edits, the records and the
// two history rules. Line numbers are read from the file.
// DDD role: ValueObject
#Documentation: {
	records: {
		edits: [
			{document: "docs/API.md", line: 1106, required: "add the four per-kind codes; keep RESOURCE_GONE as not emitted here"},
			{document: "docs/API.md", lines: "1084-1087", required: "state the four checked kinds and the three permissive ones"},
			{document: "docs/OPERATIONS.md", lines: "53, 176-213", required: "missing id is the per-kind code; sub_open never returns RESOURCE_GONE and accepts a lifecycle-Closed row; resources/subscribe owns RESOURCE_GONE"},
			{document: "docs/LLM_GUIDE.md", lines: "921, 1429, 1439, 1449, 1469-1472", required: "per-kind code for a missing id; sub_open does not return RESOURCE_GONE; the four When lines name sub_open"},
			{document: "docs/RESOURCES.md", lines: "24-29, 98", required: "live id needed for the four repository kinds"},
			{document: "CHANGELOG.md", section: "## [Unreleased] / ### Changed", required: "one bullet: refusal and zero cost"},
			{document: "docs/MIGRATION.md", section: "## v7.1.3 → unreleased (dependency security update), heading 880, beside the feature-006 bullet at 882", required: "one bullet: per-kind refusal, no lane opened, wire unchanged"},
			{document: "doc/arch/functional/functional.md", lines: "247-251", required: "repaired: four checked, three permissive"},
			{document: "doc/arch/functional/functional.md", section: "F8 table, after line 341", required: "one row bound to the FR-001 and FR-002 tests"},
			{document: "doc/arch/runbooks/incident-push-delivery-lost.md", lines: "11, 101-111", required: "a refused call returns no id; triage starts with the code"},
		]
		history_unchanged: [
			"docs/MIGRATION.md:567",
			"docs/MIGRATION.md:625",
			"docs/MIGRATION.md:641",
			"docs/MIGRATION.md:826",
			"docs/adr/0004-channel-mux-fairness.md:136",
			"docs/adr/0004-channel-mux-fairness.md:137",
		]
		section_check: "API.md current reference; OPERATIONS.md current runbook; LLM_GUIDE.md current guide; RESOURCES.md current reference; CHANGELOG.md Unreleased; functional.md and the runbook current corpus; MIGRATION.md 826 is history, new bullet under the current section at 880"
		session_entries: [
			{defect: "the debouncer sub_open starts is never stopped", cites: ["src/application/subscription_admin.rs:148-150", "src/adapters/subscription/memory_registry.rs:651-657", "src/adapters/subscription/memory_registry.rs:371-410", "src/adapters/subscription/memory_registry.rs:459-468"], test: "unknown_resource_lane_receives_delivery_once_the_resource_appears", source: "not in the tree; /var/tmp/ssh-mcp/sub-repro.patch", pins: "delivery needs the debouncer; no test observes the task"},
			{defect: "lanes outlive the shell, the command and the peer", cites: ["src/application/close_shell.rs:102-114", "src/application/execute_command.rs:105-115", "src/adapters/subscription/subscriber_lane.rs:205-208"], tests: ["closing_the_shell_leaves_its_lane_open", "completed_command_leaves_its_lane_open", "closed_peer_leaves_its_lane_open", "open_and_end_cycles_leak_one_lane_each_until_the_cap_refuses", "open_and_end_cycles_exhaust_the_global_cap"], source: "not in the tree; /var/tmp/ssh-mcp/sub-repro.patch", pins: "8 lanes after 8 cycles; 9th refused at the fixture cap; 64 lanes; 65th refused", numbers: ["SSH_MAX_SUBS_PER_URI 16", "SSH_MAX_SUBS_TOTAL 1024", "fixture 8 and 64"]},
			{defect: "the session health lane survives disconnect", cites: ["src/application/disconnect_session.rs:175-188", "src/application/disconnect_session.rs:243", "src/application/disconnect_session.rs:259", "src/application/disconnect_session.rs:280"], test: "session_disconnect_closes_command_shell_and_transfer_lanes_but_not_health", source: "not in the tree; /var/tmp/ssh-mcp/sub-repro.patch", pins: "only session://sess-d/health remains"},
			{defect: "sub_open never calls LifecyclePolicyPort::on_subscribe, so it never returns RESOURCE_GONE and accepts a lifecycle-Closed row", cites: ["src/application/subscription_admin.rs:135-162", "docs/OPERATIONS.md:176-213", "docs/LLM_GUIDE.md:1469-1472"], docs_claiming: ["docs/ARCHITECTURE.md:422", "docs/adr/0004-channel-mux-fairness.md:153"], source: "no test"},
			{defect: "daemon unsubscribe sends the sub_id as a URI", cites: ["src/embed/dispatcher.rs:625-627", "src/infra/mcp/resource_handlers.rs:840-843", "src/application/unsubscribe_resource.rs:78-79"], source: "no test"},
			{defect: "DAEMON.md promises a sub_id in the subscribe ack", cites: ["docs/DAEMON.md:131", "src/embed/dispatcher.rs:587-595"], source: "no test"},
			{defect: "API.md promises a BYTES line in the sub_replay answer", cites: ["docs/API.md:1168", "src/infra/mcp/render/subscription.rs:331-339"], source: "no test"},
			{defect: "CONFIGURATION.md names a FILTER_INVALID code", cites: ["docs/CONFIGURATION.md:153", "src/adapters/subscription/filter.rs:56-59"], source: "no test"},
			{defect: "a typo in forward, serial or rsync opens a dead lane", cites: ["src/adapters/repo/dashmap/forward.rs:57", "src/adapters/repo/dashmap/rsync.rs:28", "src/adapters/serial/state.rs:325"], source: "no test; out of scope by Q-9"},
		]
		item_6: {
			corpus_cites: ["doc/arch/threat-model/threat-model.md:58", "doc/arch/threat-model/threat-model.md:104", "doc/arch/observability/observability.md:181"]
			real_install: "src/composition/prod.rs:804"
			real_import:  "src/composition/prod.rs:27"
		}
	}
	catalogue_unchanged: true
}

// #Records groups the out-of-scope list with its places.
// DDD role: ValueObject
#Records: {
	out_of_scope: [
		{defect: "peer GC of the hexagonal registry and its lifecycle debit", recorded: "session.md defect list; F7"},
		{defect: "unsubscribe_sync race", recorded: "session.md defect list; F7"},
		{defect: "per-session producer forwarder", recorded: "session.md:114-116"},
		{defect: "lane lifetime is not enforced", recorded: "session.md:128-136; F2"},
		{defect: "sub_replay and the never-started mux drain", recorded: "session.md:137-150; F3"},
		{defect: "filter length cap unenforced", recorded: "session.md:204-205; F4"},
		{defect: "dead error codes and the auto-close spelling", recorded: "session.md:209-222; F5"},
		{defect: "daemon subscribe op drops its parameters", recorded: "session.md:206-208; F6"},
		{defect: "daemon unsubscribe sends the sub_id as a URI", recorded: "session.md:256-260; F6"},
		{defect: "lanes outlive shell, command and peer; health lane survives disconnect", recorded: "session.md:232-250; F7"},
		{defect: "sub_open accepts a lifecycle-Closed resource with a live repository row and never returns RESOURCE_GONE", recorded: "session.md on_subscribe entry; OPERATIONS.md:176-213; LLM_GUIDE.md:1469-1472"},
		{defect: "the check/open window", recorded: "spec Out of Scope"},
		{defect: "the frozen descriptions", recorded: "session.md:219-222; F8"},
	]
}

// #Fates is the two fate tables plus the new tests.
// DDD role: ValueObject
#Fates: {
	existing: [
		{test: "subscribe_to_unknown_shell_returns_shell_not_found", file: "src/application/subscribe_resource.rs:478", fate: "kept"},
		{test: "subscribe_to_unknown_command_returns_command_not_found", file: "src/application/subscribe_resource.rs:506", fate: "kept"},
		{test: "subscribe_to_forward_uri_always_succeeds", file: "src/application/subscribe_resource.rs:545", fate: "kept"},
		{test: "subscription_admin sub_open unit tests", file: "src/application/subscription_admin.rs:702-1573", fate: "kept; guard only in composition (Q-1)"},
		{test: "v5_smoke sub_open tests", file: "tests/v5_smoke.rs:90, :583, :690, :788, :912", fate: "kept; builds SubscribeUseCase::new directly (Q-1)"},
		{test: "chaos_inline_push", file: "tests/chaos_inline_push.rs:250, :411, :463, :465", fate: "kept; builds SubscribeUseCase::new directly (Q-1)"},
		{test: "test_subscribe_invalid_filter_regex", file: "scripts/test_v5_adversarial.py:660", fate: "adapted to forward://x/events; probe fails SHELL_NOT_FOUND (Q-1)"},
		{test: "test_max_subs_per_uri_exceeded_with_low_cap", file: "scripts/test_v5_adversarial.py:719", fate: "adapted to forward://overflow-test/events; probe fails SHELL_NOT_FOUND (Q-1)"},
		{test: "test_subscribe_unsubscribe_concurrent_burst", file: "scripts/test_v5_adversarial.py:745", fate: "adapted to forward; probe fails SHELL_NOT_FOUND (Q-1)"},
		{test: "test_subscribe_returns_required_hint_line", file: "scripts/test_v5_adversarial.py:781", fate: "adapted to forward; probe fails SHELL_NOT_FOUND (Q-1)"},
		{test: "test_sub_list_during_burst_churn", file: "scripts/test_v5_adversarial.py:963", fate: "adapted to forward; probe fails SHELL_NOT_FOUND (Q-1)"},
		{test: "test_subscribe_hint_mentions_flush_bytes_knob", file: "scripts/test_v51_flush_bytes.py:64", fate: "adapted to forward://placeholder/events; probe fails COMMAND_NOT_FOUND (Q-1)"},
		{test: "chaos_v5_subscribe shell-URI tests", file: "scripts/chaos_v5_subscribe.py:139, :187, :424, :467", fate: "adapted to forward; probe fails SHELL_NOT_FOUND (Q-1)"},
	]
	patch: [
		{test: "subscribe_unknown_resource_returns_resource_not_found", fate: "proving test of FR-001; renamed subscribe_unknown_shell_is_refused"},
		{test: "unknown_resource_lane_still_consumes_a_cap_slot_and_a_stats_row", fate: "replaced by refused_sub_open_leaves_no_lane_and_no_debouncer"},
		{test: "unknown_resource_lane_receives_delivery_once_the_resource_appears", fate: "replaced by refused_then_created_resource_can_be_subscribed"},
		{test: "closing_the_shell_leaves_its_lane_open", fate: "not added; out-of-scope F7"},
		{test: "completed_command_leaves_its_lane_open", fate: "not added; out-of-scope F7"},
		{test: "session_disconnect_closes_command_shell_and_transfer_lanes_but_not_health", fate: "not added; out-of-scope F7"},
		{test: "closed_peer_leaves_its_lane_open", fate: "not added; out-of-scope F7"},
		{test: "open_and_end_cycles_leak_one_lane_each_until_the_cap_refuses", fate: "not added; out-of-scope F7"},
		{test: "open_and_end_cycles_exhaust_the_global_cap", fate: "not added; out-of-scope F7"},
		{test: "auto_close_lane_is_reaped_after_grace_window", fate: "not added; out-of-scope F2"},
		{test: "lease_lane_outlives_its_ttl", fate: "not added; out-of-scope F2"},
		{test: "oversized_filter_is_accepted_by_sub_open", fate: "not added; out-of-scope F4; URI becomes forward://big-filter/events"},
		{test: "replay_from_cursor_enqueues_a_snapshot_for_the_consumer", fate: "not added; out-of-scope F3"},
		{test: "sub_replay_answers_ok_into_a_lane_queue_nothing_drains", fate: "not added; out-of-scope F3; URI becomes forward://replay-probe/events"},
		{test: "lifetime_accepts_only_the_snake_case_spelling", fate: "not added; out-of-scope F5"},
		{test: "lag_policy_rejects_non_snake_case_and_unknown_values", fate: "not added; out-of-scope F5"},
		{test: "subscribe_op_parses_every_documented_field", fate: "not added; out-of-scope F6"},
		{test: "subscribe_lifetime_is_free_form_but_lag_policy_is_typed", fate: "not added; out-of-scope F6"},
		{test: "subscribe_op_maps_to_resources_subscribe_not_sub_open", fate: "not added; out-of-scope F6"},
		{test: "oversized_regex_compiles_even_past_the_configured_cap", fate: "not added; out-of-scope F4"},
		{test: "nested_group_bomb_compiles_under_the_timeout", fate: "not added; out-of-scope F4"},
	]
	new: [
		{test: "production_sub_subscribe_refuses_missing_resources_and_opens_forward", file: "tests/sub_open_resource_check.rs", fr: "FR-011"},
	]
}

// #Proof is FR-009 and FR-010: the gate, its floors and the validation
// baseline, all measured on the feature-006 tree.
// DDD role: ValueObject
#Proof: {
	tests: [
		{name: "subscribe_unknown_shell_is_refused", module: "crate::application::subscription_admin::tests", fr: "FR-001", asserts: "ShellNotFound with the id"},
		{name: "subscribe_unknown_command_is_refused", module: "crate::application::subscription_admin::tests", fr: "FR-001", asserts: "CommandNotFound with the id"},
		{name: "subscribe_unknown_transfer_is_refused", module: "crate::application::subscription_admin::tests", fr: "FR-001", asserts: "TransferNotFound with the id"},
		{name: "subscribe_unknown_session_is_refused", module: "crate::application::subscription_admin::tests", fr: "FR-001", asserts: "SessionNotFound with the id"},
		{name: "refused_sub_open_leaves_no_lane_and_no_debouncer", module: "crate::application::subscription_admin::tests", fr: "FR-002", asserts: "error, empty list, no activator call, cap still free"},
		{name: "refused_then_created_resource_can_be_subscribed", module: "crate::application::subscription_admin::tests", fr: "FR-001, FR-002", asserts: "refusal then success and one delivery"},
		{name: "forward_serial_and_rsync_stay_permissive", module: "crate::application::subscription_admin::tests", fr: "FR-003", asserts: "three lanes open"},
		{name: "sub_open_refusal_answers_per_kind_code", module: "crate::infra::mcp::tool_router::tests", fr: "FR-005", asserts: "run_sub_subscribe over the guarded use case: SUB_OPEN: ERROR, per-kind REASON and DETAIL, structured code; fails on base"},
		{name: "sub_open_refusal_renders_per_kind_code", module: "crate::infra::mcp::tool_router::tests", fr: "FR-005", asserts: "renderer mapping only; passes on base"},
		{name: "production_sub_subscribe_refuses_missing_resources_and_opens_forward", module: "tests/sub_open_resource_check.rs", fr: "FR-011", asserts: "build_use_cases: four refusals, one forward success; fails on base"},
		{name: "completed_command_row_is_accepted", module: "crate::application::subscription_admin::tests", fr: "FR-012", asserts: "terminal command row accepted; passes on base"},
		{name: "completed_transfer_row_is_accepted", module: "crate::application::subscription_admin::tests", fr: "FR-012", asserts: "terminal transfer row accepted; passes on base"},
		{name: "refused_sub_open_is_not_cached_under_its_idempotency_key", module: "crate::infra::mcp::tool_router::tests", fr: "FR-013", asserts: "refusal not cached; retry re-runs"},
	]
	commands: [
		"cargo fmt --all -- --check",
		"cargo check --all-features --all-targets",
		"cargo clippy --all-features -- -D warnings",
		"cargo build --bins",
		"cargo nextest run --test-threads num-cpus --lib",
		"cargo nextest run --test-threads num-cpus --tests --features test-fixtures",
		"cargo build --no-default-features --target-dir target/no-default-features",
		"/root/dev/ssh-mcp/.venv/bin/python -m pytest -rs scripts/test_http.py",
		"/root/dev/ssh-mcp/.venv/bin/python -m pytest -rs scripts/test_transfer_progress.py",
		"/root/dev/ssh-mcp/.venv/bin/python -m pytest -rs scripts/test_v7_rsync_http.py",
		"/root/dev/ssh-mcp/.venv/bin/python -m pytest -rs scripts/test_v47_subscriptions.py",
		"/root/dev/ssh-mcp/.venv/bin/python -m pytest -rs scripts/test_v47_idempotency.py",
		"PYTHONPATH=scripts SSH_MCP_STDIO_BIN=$PWD/target/debug/ssh-mcp-stdio SSH_MCP_BIN=$PWD/target/debug/ssh-mcp SSH_MCP_TAIL_BIN=$PWD/target/debug/ssh-mcp-tail /root/dev/ssh-mcp/.venv/bin/python -m pytest -q scripts/test_v5_adversarial.py",
		"PYTHONPATH=scripts SSH_MCP_STDIO_BIN=$PWD/target/debug/ssh-mcp-stdio SSH_MCP_BIN=$PWD/target/debug/ssh-mcp SSH_MCP_TAIL_BIN=$PWD/target/debug/ssh-mcp-tail /root/dev/ssh-mcp/.venv/bin/python -m pytest -q scripts/test_v51_flush_bytes.py",
		"cargo audit",
		"make tools-export-check",
	]
	policy: {
		baseline: {
			exit_status:         6
			errors:              68
			warnings:            0
			feature_007_finding: 0
			rule_counts: {
				"fqcn.relative-super":            46
				"branch.else":                    6
				"pin.ci.unpinned-action":         5
				"pin.docker.unpinned-image":      2
				"readme.mermaid.missing":         1
				"readme.index.missing":           1
				"readme.image.missing":           1
				"readme.arch-link.missing":       1
				"pin.cargo.lockfile-missing":     1
				"llms-txt.file.missing":          1
				"fqcn.glob-import":               1
				"crew.roster.dangling":           1
				"ci.gitlab.missing":              1
			}
		}
		floors: {
			lib_tests:         2154
			integration_tests: 2329
		}
		rules: {
			no_lint_disabled:           true
			no_allow_added:             true
			no_test_ignored_or_removed: true
			catalogue_unchanged:        true
			adapted_scripts:            true
		}
	}
}

// #SubOpenResourceCheck binds the check, the policy, the records and the proof.
// DDD role: ValueObject
#SubOpenResourceCheck: {
	behavior:  {
		check:      #Check
		policy:     #Policy
		no_lane:    #NoLane
		production: #Production
	}
	documentation: #Documentation
	records:       #Records
	fates:         #Fates
	proof:         #Proof
	provenance: "per-kind code chosen by the orchestrator under the owner's instruction to proceed; recorded for the owner's review"
	clarifications: {
		open: [
			{q: "Q-1 existing unseeded tests and their fate", status: "answered; 25 call sites, guard only in composition, six script cases adapted"},
			{q: "Q-2 idempotency cache of a refusal", status: "answered; not cached, retry re-runs"},
			{q: "Q-3 daemon observation", status: "answered; no NDJSON shape change"},
			{q: "Q-4 HTTP observation", status: "answered; one server and fresh repos per session"},
			{q: "Q-5 where the single policy lives", status: "plan decision; constraints recorded"},
			{q: "Q-6 exact test totals", status: "answered; floors 2154 and 2329, no removal"},
			{q: "Q-7 where the FR-011 composition test lives", status: "answered; tests/ auto-discovered, proven in probe"}
			{q: "Q-8 production callers of execute", status: "answered; run_sub_subscribe only"}
			{q: "Q-9 permissive kinds storage", status: "answered; forward/rsync ports, serial global registry; dead lane recorded"}
			{q: "Q-10 what exists means", status: "answered; row presence, terminal rows accepted"},
			{q: "Q-1b how the three out-of-gate scripts run and whether they pass today", status: "answered; adversarial 63 passed, flush-bytes 1 passed, chaos 7 pre-existing failures"},
		]
	}
}
