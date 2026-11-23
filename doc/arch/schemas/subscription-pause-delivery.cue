// DDD role: ValueObject

package schemas

// #DeliveryPaths is FR-001 and FR-004: the two gated paths, the second
// gate at the dispatch consumer, and the isolation the gate must keep.
// DDD role: ValueObject
#DeliveryPaths: {
	legacy: {
		name:  "legacy_resource_updated"
		entry: "src/adapters/subscription/lane_bridge.rs:280"
		gate:  "lane.paused Acquire load before the notifier call"
	}
	inline: {
		name:  "inline_ssh_output"
		entry: "src/adapters/subscription/lane_bridge.rs:291"
		gate:  "lane.paused Acquire load before the notifier call"
	}
	dispatch_consumer_gate: "src/adapters/subscription/lane_bridge.rs:470"
	isolation: {
		other_lane_same_uri:  "delivers"
		other_peer_same_uri:  "delivers"
		peerless_legacy_lane: "unaffected (pinned residual, FR-011)"
	}
}

// #ResumeSignal is FR-002: one legacy notification on a true -> false
// transition, and none on a running lane.
// DDD role: ValueObject
#ResumeSignal: {
	method:                "notifications/resources/updated"
	transition:            "true -> false"
	count_on_transition:   1
	count_on_running:      0
	counts_in_events_sent: true
	failure_is_fatal:      false
	failure_log_level:     "debug"
}

// #CursorSemantics is FR-003: compose while paused, ship after resume.
// DDD role: ValueObject
#CursorSemantics: {
	composed_while_paused:          true
	fragments_shipped_while_paused: false
	seq_after_resume:               "previous seq plus the dropped fragment count"
	cursor_after_after_resume:      "absolute produced offset"
	payload_store:                  "none; the resource ring holds the bytes"
}

// #EdgeCases is the complete FR-005 set, with today's outcome and the
// required one.
// DDD role: ValueObject
#EdgeCases: [
	{case: "pause_twice", today: "OK", required: "OK"},
	{case: "resume_running", today: "OK, no notification", required: "OK, no notification"},
	{case: "unknown_sub_id", today: "SUB_NOT_FOUND", required: "SUB_NOT_FOUND"},
	{case: "closed_while_paused", today: "SUB_NOT_FOUND", required: "SUB_NOT_FOUND"},
	{case: "resource_ends_while_paused", today: "terminal wake suppressed", required: "resume notification, terminal status on read"},
]

// #Observability is FR-006.
// DDD role: ValueObject
#Observability: {
	sub_list: {
		renders_paused: true
	}
	counters: {
		frozen_while_paused: ["events_sent", "bytes_sent", "inline_events_sent", "inline_bytes_sent"]
		queue_depth_source: "SubscriberLaneAdapter::produce (no production caller)"
	}
	new_signals: {
		metric:    false
		log_event: false
		span:      false
	}
}

// #Tests is the complete proof set of the specification.
// DDD role: ValueObject
#Tests: [
	{name: "paused_lane_receives_no_delivery_until_resume", kind: "tokio-test", asserts: "no resource or inline event while paused; one of each after resume"},
	{name: "resume_after_a_quiet_pause_sends_exactly_one_notification", kind: "tokio-test", asserts: "one catch-up notification"},
	{name: "resume_of_a_running_lane_sends_no_extra_notification", kind: "tokio-test", asserts: "zero catch-up notifications"},
	{name: "paused_inline_lane_keeps_cursor_and_seq_monotonic_across_the_gap", kind: "tokio-test", asserts: "seq 0 then 2, cursor_after 5 then 14, second payload cccc"},
	{name: "pause_does_not_gate_another_lane_on_the_same_uri", kind: "tokio-test", asserts: "only the unpaused peer is recorded"},
	{name: "pause_unknown_lane_returns_sub_not_found", kind: "tokio-test", asserts: "SUB_NOT_FOUND"},
	{name: "resume_of_a_lane_closed_while_paused_returns_sub_not_found", kind: "tokio-test", asserts: "SUB_NOT_FOUND"},
	{name: "paused_lane_counters_do_not_advance", kind: "tokio-test", asserts: "four delivery counters frozen while paused; events_sent 2 and bytes_sent 2 after resume and a 2-byte write"},
	{name: "resources_subscribe_lane_is_listed_but_pause_does_not_gate_the_peer_fanout", kind: "tokio-test", asserts: "sub_list carries the sub_id, the peer event arrives while paused, counters stay zero"},
	{name: "adapter_and_bridge_are_released_after_drop", kind: "tokio-test", asserts: "both Weak upgrades are dead after the strong handles drop"},
	{name: "resume_of_a_paused_lane_notifies_once_with_its_sub_id", kind: "tokio-test", asserts: "one notify_lane call carrying the lane sub_id"},
	{name: "resume_of_a_running_lane_does_not_notify", kind: "tokio-test", asserts: "no notify_lane call"},
	{name: "resume_unknown_does_not_notify", kind: "tokio-test", asserts: "SUB_NOT_FOUND and no notify_lane call"},
	{name: "paused_lane_inline_only_path_is_gated", kind: "tokio-test", asserts: "the inline-only producer entry sends nothing while paused"},
	{name: "prepare_inline_dispatch_skips_a_paused_lane", kind: "tokio-test", asserts: "no dispatch is enqueued while paused and seq jumped the paused write"},
	{name: "queued_inline_dispatch_is_dropped_when_the_lane_is_paused", kind: "tokio-test", asserts: "a dispatch dequeued after the pause is dropped at ship_one"},
	{name: "list_render_emits_one_line_per_summary", kind: "test", asserts: "paused=true and paused=false render"},
	{name: "pause_resume_renders_distinct_state_labels", kind: "test", asserts: "STATE: paused, STATE: resumed and both structured twins"},
	{name: "pause_structured_paused_is_true", kind: "test", asserts: "structured paused is true"},
	{name: "resume_structured_paused_is_false", kind: "test", asserts: "structured paused is false"},
	{name: "pause_resume_round_trip", kind: "tokio-test", asserts: "the flag round trip and the summary reads"},
]

// #Gate is FR-008 and FR-010: the verified-change gate, its floors, the
// no-weakening rule and the validation baseline.
// DDD role: ValueObject
#Gate: {
	commands: [
		"cargo fmt --all -- --check",
		"cargo clippy --all-features -- -D warnings",
		"cargo nextest run --test-threads num-cpus --lib",
		"cargo nextest run --test-threads num-cpus --tests --features test-fixtures",
		"cargo build --no-default-features --target-dir target/no-default-features",
		"/root/dev/ssh-mcp/.venv/bin/python -m pytest -rs scripts/test_http.py",
		"/root/dev/ssh-mcp/.venv/bin/python -m pytest -rs scripts/test_transfer_progress.py",
		"/root/dev/ssh-mcp/.venv/bin/python -m pytest -rs scripts/test_v7_rsync_http.py",
		"/root/dev/ssh-mcp/.venv/bin/python -m pytest -rs scripts/test_v47_subscriptions.py",
		"/root/dev/ssh-mcp/.venv/bin/python -m pytest -rs scripts/test_v47_idempotency.py",
		"cargo audit",
		"make tools-export-check",
	]
	policy: {
		baseline: {
			exit_status:         6
			errors:              68
			warnings:            0
			feature_005_finding: 0
			rule_counts: {
				"fqcn.relative-super":            46
				"branch.else":                    6
				"llm-context.regeneration.drift": 1
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
			lib_tests:         2141
			integration_tests: 2315
		}
		rules: {
			no_lint_disabled:           true
			no_allow_added:             true
			no_test_ignored_or_removed: true
			catalogue_unchanged:        true
		}
	}
}

// #Documentation is FR-009: the records, the repaired defect entry and
// the five corrected sentences.
// DDD role: ValueObject
#Documentation: {
	records: [
		{file: "CHANGELOG.md", role: "changelog"},
		{file: "docs/MIGRATION.md", role: "migration"},
		{file: "session.md", role: "defect_repaired"},
		{file: "docs/API.md", role: "inline_guidance"},
		{file: "session.md", role: "residual_entry"},
		{file: "session.md", role: "open_item_extension"},
		{file: "src/composition/prod.rs", role: "wiring"},
		{file: "src/application/subscription_admin.rs", role: "resume_orchestration"},
		{file: "docs/adr/0004-channel-mux-fairness.md:136", role: "historical_unchanged"},
		{file: "docs/adr/0004-channel-mux-fairness.md:137", role: "historical_unchanged"},
		{file: "docs/MIGRATION.md:631", role: "historical_unchanged_v5_notes"},
		{file: "docs/MIGRATION.md:632", role: "historical_unchanged_v5_notes"},
	]
	edits: [
		{document: "docs/API.md", line: 1126, today: "Suspend the lane drain loop. Producer keeps emitting; the mpsc fills under the lane's lag policy.", required: "Suspend delivery for the lane. The producer and the resource ring keep running; the consumer reads the missed range from its cursor at resume. Gates sub_open lanes only."},
		{document: "docs/API.md", line: 1138, today: "Resume the drain loop after `sub_pause`.", required: "Resume delivery after `sub_pause` and emit one catch-up `notifications/resources/updated` for the lane."},
		{document: "docs/OPERATIONS.md", line: 79, today: "suspended the drain loop", required: "suspended delivery for that lane"},
		{document: "docs/OPERATIONS.md", line: 261, today: "the subscriber lane mpsc is paused (`sub_pause`)", required: "delivery for that lane is paused"},
		{document: "docs/ARCHITECTURE.md", line: 376, today: "suspend drain without disconnecting", required: "suspend delivery without disconnecting"},
	]
}

// #SubscriptionPauseDelivery binds the delivery rules, the frozen surface,
// the proof set and the records of feature 005.
// DDD role: ValueObject
#SubscriptionPauseDelivery: {
	delivery: {
		paths:         #DeliveryPaths
		resume:        #ResumeSignal
		cursor:        #CursorSemantics
		edge_cases:    #EdgeCases
		observability: #Observability
		residual: {
			lane:         "resources/subscribe opens a peerless lane"
			reachable_by: "sub_list returns its sub_id; sub_pause returns OK"
			delivery:     "per-peer fan-out (memory_registry.rs:985-987), ungated"
			pinned_by:    "crate::adapters::subscription::memory_registry::tests::resources_subscribe_lane_is_listed_but_pause_does_not_gate_the_peer_fanout"
			closed_by:    "the feature that associates a (peer, uri) pair with its lane"
		}
	}
	surface: {
		tool_descriptions: ["src/infra/mcp/tool_router.rs:2441", "src/infra/mcp/tool_router.rs:2465"]
		argument_descriptors: ["src/infra/mcp/args/subscription.rs:116-128"]
		response_lines: ["SUB_PAUSE: OK", "SUB_RESUME: OK", "STATE: paused", "STATE: resumed"]
		new_error_code:          false
		new_notification_method: false
		new_env_var:             false
	}
	proof: {
		tests: #Tests
		gate:  #Gate
	}
	documentation: #Documentation
	provenance:    "taken in this session on the owner's instruction to carry the whole update plan forward; recorded for the owner's review"
}
