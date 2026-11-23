// DDD role: ValueObject

package schemas

// #Removal is FR-001, FR-002 and FR-005: the declarations that go, the
// reference set each `findReferences` returned before the spec was written,
// and the pin that is replaced.
// DDD role: ValueObject
#Removal: {
	facts: {
		symbols: {
			subscribe_outcome_field:   "src/application/subscribe_resource.rs:60"
			subscribe_lane_field:      "src/application/subscribe_resource.rs:90"
			subscribe_builder:         "src/application/subscribe_resource.rs:128"
			subscribe_open_helper:     "src/application/subscribe_resource.rs:174-188"
			unsubscribe_request_field: "src/application/unsubscribe_resource.rs:41"
			unsubscribe_lane_field:    "src/application/unsubscribe_resource.rs:64"
			unsubscribe_builder:       "src/application/unsubscribe_resource.rs:83"
		}
		references: {
			subscribe_outcome_field:   ["src/application/subscribe_resource.rs:170", "src/application/subscribe_resource.rs:625", "src/application/subscribe_resource.rs:659", "src/infra/mcp/resource_handlers.rs:825"]
			subscribe_lane_field:      ["src/application/subscribe_resource.rs:118", "src/application/subscribe_resource.rs:129", "src/application/subscribe_resource.rs:180"]
			subscribe_builder:         ["src/composition/prod.rs:524", "src/application/subscribe_resource.rs:652"]
			unsubscribe_request_field: ["src/application/unsubscribe_resource.rs:107", "src/application/unsubscribe_resource.rs:228", "src/application/unsubscribe_resource.rs:352", "src/application/unsubscribe_resource.rs:389", "src/application/unsubscribe_resource.rs:439", "src/infra/mcp/resource_handlers.rs:866"]
			unsubscribe_lane_field:    ["src/application/unsubscribe_resource.rs:77", "src/application/unsubscribe_resource.rs:84", "src/application/unsubscribe_resource.rs:106"]
			unsubscribe_builder:       ["src/composition/prod.rs:529", "src/application/unsubscribe_resource.rs:346", "src/application/unsubscribe_resource.rs:381"]
		}
		branches: {
			lane_close: "src/application/unsubscribe_resource.rs:106-113"
			log:        "src/infra/mcp/resource_handlers.rs:824-832"
		}
		residual_tests: {
			replaced:    "crate::adapters::subscription::memory_registry::tests::resources_subscribe_lane_is_listed_but_pause_does_not_gate_the_peer_fanout"
			replacement: "crate::application::subscribe_resource::tests::resources_subscribe_records_no_lane_and_delivers_once"
			source:      "replacement is a new test; the replaced one is in the tree"
		}
		fates: {
			existing: [
				{test: "subscribe_outcome_sub_id_is_none_when_lane_not_wired", file: "src/application/subscribe_resource.rs:615", fate: "removed; replaced by resources_subscribe_records_no_lane_and_delivers_once"},
				{test: "subscribe_with_lane_returns_sub_id_on_outcome", file: "src/application/subscribe_resource.rs:628", fate: "removed; replaced by resources_subscribe_records_no_lane_and_delivers_once"},
				{test: "unsubscribe_routes_to_canonical_uri", file: "src/application/unsubscribe_resource.rs:232", fate: "adapted"},
				{test: "unsubscribe_is_idempotent_for_unknown_peer", file: "src/application/unsubscribe_resource.rs:246", fate: "adapted"},
				{test: "malformed_uri_returns_invalid_argument", file: "src/application/unsubscribe_resource.rs:263", fate: "adapted"},
				{test: "unsubscribe_routes_each_scheme_to_canonical_form", file: "src/application/unsubscribe_resource.rs:278", fate: "adapted"},
				{test: "missing_id_returns_invalid_argument", file: "src/application/unsubscribe_resource.rs:295", fate: "adapted"},
				{test: "unsubscribe_carries_peer_id_through_to_registry", file: "src/application/unsubscribe_resource.rs:308", fate: "adapted"},
				{test: "unsubscribe_with_lane_closes_matching_sub_id", file: "src/application/unsubscribe_resource.rs:321", fate: "removed; replaced by resources_unsubscribe_removes_the_registry_entry and resources_unsubscribe_leaves_a_sub_open_lane_open"},
				{test: "unsubscribe_tolerates_unknown_sub_id_when_lane_wired", file: "src/application/unsubscribe_resource.rs:367", fate: "removed; replaced by resources_unsubscribe_removes_the_registry_entry"},
				{test: "release_resource_reaches_closed_via_production_unsubscribe_path", file: "src/application/unsubscribe_resource.rs:398", fate: "adapted"},
				{test: "resources_subscribe_lane_is_listed_but_pause_does_not_gate_the_peer_fanout", file: "src/adapters/subscription/memory_registry.rs:1787", fate: "replaced by resources_subscribe_records_no_lane_and_delivers_once"},
			]
			patch: [
				{test: "seventeenth_resources_subscribe_with_interleaved_unsubscribe_succeeds", fate: "becomes the proving test of FR-011, name kept"},
				{test: "repeated_resources_subscribe_inflates_refcount_and_lanes_only", fate: "replaced by repeated_resources_subscribe_counts_once_in_the_lifecycle"},
				{test: "lane_limit_failure_leaves_a_live_subscription_that_still_delivers", fate: "replaced by lifecycle_refusal_leaves_no_registry_entry_and_no_delivery"},
				{test: "resources_unsubscribe_closes_the_lane_it_opened", fate: "replaced by resources_subscribe_records_no_lane_and_delivers_once"},
				{test: "resources_unsubscribe_keeps_the_lane_it_does_not_know_about", fate: "replaced by resources_unsubscribe_removes_the_registry_entry"},
				{test: "resources_subscribe_lane_delivers_once_and_never_through_the_bridge", fate: "replaced by resources_subscribe_records_no_lane_and_delivers_once"},
				{test: "peer_gc_and_drop_peer_leave_the_resources_subscribe_lane_open", fate: "replaced by resources_subscribe_records_no_lane_and_delivers_once"},
				{test: "gc_debits_the_lifecycle_refcount_of_the_reaped_peer", fate: "not added; out-of-scope defect, stays in the patch"},
				{test: "concurrent_unsubscribe_does_not_swallow_a_new_subscriber", fate: "not added; out-of-scope race, stays in the patch"},
				{test: "installing_a_second_forwarder_keeps_the_first_alive", fate: "not added; out-of-scope defect"},
				{test: "subscribe_unknown_resource_returns_resource_not_found", fate: "not added; out of scope"},
				{test: "auto_close_lane_is_reaped_after_grace_window", fate: "not added; out of scope"},
			]
		}
	}
	composition_wiring: ["src/composition/prod.rs:517-526", "src/composition/prod.rs:529-531"]
	kept:               ["SubscriberLaneAdapter", "LaneAdmin", "LanePolicy", "sub_open", "sub_close", "sub_pause", "UnsubscribeUseCase"]
	residual_passages: [
		{document: "doc/arch/functional/functional.md", lines: "229-236", role: "limitation_bullet"},
		{document: "doc/arch/functional/functional.md", lines: "341", role: "f8_table_row"},
		{document: "doc/arch/runbooks/incident-push-delivery-lost.md", lines: "106-111", role: "runbook_step_8"},
		{document: "docs/API.md", lines: "1128", role: "sub_pause_note"},
		{document: "session.md", lines: "112-117", role: "defect_repaired"},
	]
}

// #Port is FR-003 and FR-004: the registry reports creation or refresh, only
// creation touches the lifecycle, and a failed lifecycle step removes the
// created entry.
// DDD role: ValueObject
#Port: {
	signal: {
		method:                 "SubscriberRegistryAsync::subscribe"
		declaration:            "src/ports/subscriber_registry.rs:157-163"
		disposition: {
			created:   ["src/adapters/subscription/memory_registry.rs:584-609"]
			refreshed: ["src/adapters/subscription/memory_registry.rs:594"]
		}
		lifecycle_increment_on: "created only"
		debouncer_signal:       "the existing first-subscriber boolean at src/adapters/subscription/memory_registry.rs:596 stays separate; the pair's prior existence is read before the retain at :594"
	}
	implementors: {
		production:         ["src/adapters/subscription/memory_registry.rs:809"]
		test_stubs:         ["src/application/subscribe_resource.rs:358", "src/application/unsubscribe_resource.rs:186"]
		production_callers: ["src/application/subscribe_resource.rs:160"]
	}
	discovery: "goToImplementation returns nothing for the macro-generated alias; text search `SubscriberRegistryAsync for` returns the three implementors above"
	compensation: {
		step:      "LifecyclePolicyPort::on_subscribe"
		call_site: "src/application/subscribe_resource.rs:164"
		error:     "DomainError::ResourceGone"
		condition: "the tracked resource is Closed at the post-increment reconciliation (src/adapters/lifecycle/refcount.rs:482-488)"
		action:    "SubscriberRegistryAsync::unsubscribe the created pair, then return the error"
		scope:     "created only; a refresh leaves the pre-existing entry in place"
		concurrency: "the rollback removes the record even after a concurrent refresh of the same pair; the resource is Closed, so at most one flush already in flight can notify (memory_registry.rs:977-981)"
	}
}

// #Security is the security answer set of the specification.
// DDD role: ValueObject
#Security: {
	data:           "no bytes move; the URI and the peer handle only"
	auth:           "no authenticated surface"
	input:          "the URI parser is unchanged; sub_id is removed"
	cryptography:   "none added"
	logging:        "one info line removed: resources/subscribe: SubId synthesised"
	error_exposure: "no code or message changes"
}

// #Documentation is FR-006, FR-009 and FR-010: the corrections, the records
// and the two new defect entries. History stays as written.
// DDD role: ValueObject
#Documentation: {
	edits: [
		{document: "docs/LLM_GUIDE.md", line: 69, today: "Every sub_open (or legacy resources/subscribe) returns a sub_id …", required: "only sub_open returns a sub_id; a legacy subscription is peer-keyed and has no lane"},
		{document: "docs/LLM_GUIDE.md", line: 73, today: "only explicit sub_close or peer-transport disconnect closes it", required: "sub_close closes a sub_open lane; resources/unsubscribe removes the legacy subscription"},
		{document: "docs/RESOURCES.md", lines: "291-292", today: "unsubscribe / lane_admin.close(sub_id)", required: "resources/unsubscribe removes the registry entry; only sub_close reaches a lane"},
		{document: "docs/ARCHITECTURE.md", line: 780, today: "Per (SubId, Uri) cursor in v5; (PeerId, Uri) synthesised for legacy hosts", required: "the legacy method keeps only the (PeerId, Uri) cursor and opens no lane"},
		{document: "doc/arch/functional/functional.md", lines: "203-205", today: "The legacy resources/subscribe path and the lane path share the registry", required: "resources/subscribe uses the registry only"},
		{document: "docs/OPERATIONS.md", line: 71, today: "the call returned a sub_id", required: "only sub_open returns a sub_id"},
		{document: "docs/LLM_GUIDE.md", lines: "236-238", today: "Track every sub_id returned by sub_open (or the legacy resources/subscribe)", required: "sub_open returns the id sub_close takes; a legacy subscription is ended with resources/unsubscribe"},
		{document: "docs/RESOURCES.md", lines: "266-272", today: "one message (or resources/subscribe), subscribe / register lane, ack (SUB_ID)", required: "two paths: sub_open registers a lane and acks SUB_ID; resources/subscribe subscribes and returns the empty result"},
		{document: "README.md", line: 32, today: "Each resources/subscribe mints a SubId with its own mpsc channel, lag policy, filter, replay and stats", required: "sub_open mints the id and lane; a legacy subscription is peer-keyed with no lane"},
		{document: "src/application/subscribe_resource.rs", lines: "56-60", today: "Surfaced via _meta.sub_id on the rmcp resources/subscribe response", required: "the outcome carries the canonical URI only"},
		{document: "CHANGELOG.md", section: "## [Unreleased] / ### Changed", today: "no entry", required: "one bullet: no lane is opened, no sub_list row and no lane slot"},
		{document: "docs/MIGRATION.md", section: "## v7.1.3 → unreleased (dependency security update)", today: "no entry", required: "one bullet: what a host observes; the wire is unchanged"},
	]
	history_unchanged: [
		"docs/MIGRATION.md:625",
		"docs/MIGRATION.md:641",
		"docs/adr/0004-channel-mux-fairness.md:136",
		"docs/adr/0004-channel-mux-fairness.md:137",
	]
	catalogue_checked: {
		unchanged: true
		mentions: ["docs/TOOL_CATALOG.md:673", "docs/TOOL_CATALOG.md:1125", "docs/TOOL_CATALOG.md:1166", "docs/TOOL_CATALOG.md:1225"]
		check: "make tools-export-check"
	}
	session_entries: [
		{defect: "PeerGcUseCase never invoked", cites: ["src/application/peer_gc.rs:68-71", "src/composition/prod.rs:533", "src/composition/prod.rs:805-817"], test: "crate::adapters::subscription::memory_registry::tests::gc_debits_the_lifecycle_refcount_of_the_reaped_peer", source: "not in the tree; /var/tmp/ssh-mcp/resub-repro.patch", failure: "assertion left == right failed: the reaped peer's subscription must be debited from the lifecycle (left 1, right 0)"},
		{defect: "unsubscribe_sync race", cites: ["src/adapters/subscription/memory_registry.rs:458-467"], test: "crate::adapters::subscription::memory_registry::tests::concurrent_unsubscribe_does_not_swallow_a_new_subscriber", source: "not in the tree; /var/tmp/ssh-mcp/resub-repro.patch", failure: "round 1: concurrent unsubscribe swallowed the new subscriber"},
	]
}

// #Records groups the documents and the out-of-scope list.
// DDD role: ValueObject
#Records: {
	documentation: #Documentation
	out_of_scope: [
		{defect: "peer GC of the hexagonal registry and its lifecycle debit", recorded: "session.md defect list; FR-010 entry"},
		{defect: "unsubscribe_sync race", recorded: "session.md defect list; FR-010 entry"},
		{defect: "per-session producer forwarder", recorded: "session.md:105-107"},
		{defect: "sub_open does not check the resource", recorded: "session.md:122-123"},
		{defect: "lane lifetime is not enforced", recorded: "session.md:118-119"},
		{defect: "daemon subscribe op drops its parameters", recorded: "session.md:160-162"},
		{defect: "entry/lifecycle window on a concurrent unsubscribe", recorded: "session.md defect list; subscribe_resource.rs:159-164, unsubscribe_resource.rs:114-123, refcount.rs:546-555; Q-7"},
		{defect: "registry SubId and per-SubId cursors have no production reader", recorded: "memory_registry.rs:265-313, :599-608, :825; Q-10"},
	]
}

// #Proof is FR-008 and FR-009: the verified-change gate, its floors and the
// validation baseline, both measured on the feature-005 tree, and the proof
// set. Every test is a `#[tokio::test]`, so every scenario stays `@planned`;
// every test is new except `prod_server_exposes_full_tool_surface`, and the
// FR-011 test comes from the patch under the same name.
// DDD role: ValueObject
#Proof: {
	tests: [
		{name: "resources_subscribe_records_no_lane_and_delivers_once", module: "crate::application::subscribe_resource::tests", fr: "FR-001", asserts: "list_subs empty, one peer event, zero lane counters"},
		{name: "resources_unsubscribe_removes_the_registry_entry", module: "crate::application::unsubscribe_resource::tests", fr: "FR-002", asserts: "snapshot empty, second unsubscribe succeeds"},
		{name: "resources_unsubscribe_leaves_a_sub_open_lane_open", module: "crate::application::unsubscribe_resource::tests", fr: "FR-002", asserts: "the sub_open lane survives"},
		{name: "repeated_resources_subscribe_counts_once_in_the_lifecycle", module: "crate::application::subscribe_resource::tests", fr: "FR-003", asserts: "one entry, refcount 1 then 0, no lane"},
		{name: "lifecycle_refusal_leaves_no_registry_entry_and_no_delivery", module: "crate::application::subscribe_resource::tests", fr: "FR-004", asserts: "ResourceGone, empty snapshot, no event"},
		{name: "prod_server_exposes_full_tool_surface", module: "tests/v4_smoke.rs", fr: "FR-007", asserts: "the tool catalogue stays complete", source: "tree"},
		{name: "seventeenth_resources_subscribe_with_interleaved_unsubscribe_succeeds", module: "crate::application::subscribe_resource::tests", fr: "FR-011", asserts: "17 cycles on one URI all succeed at the default cap of 16", source: "patch, added under the same name"},
	]
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
			feature_006_finding: 0
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
			lib_tests:         2142
			integration_tests: 2316
		}
		totals_arithmetic: "2141 - 5 removed-or-replaced + 6 added = 2142 lib; 2315 - 5 + 6 = 2316 with integration"
		rules: {
			no_lint_disabled:           true
			no_allow_added:             true
			no_test_ignored_or_removed: true
			catalogue_unchanged:        true
			no_wire_change:             true
		}
	}
}

// #ResourcesSubscribeLaneRemoval binds the change, the records and the proof.
// DDD role: ValueObject
#ResourcesSubscribeLaneRemoval: {
	removal:    #Removal
	port:       #Port
	security:   #Security
	records:    #Records
	proof:      #Proof
	provenance: "design 2 chosen by the orchestrator under the owner's instruction to proceed; recorded for the owner's review"
	clarifications: {
		answers: [
			{q: "Q-1 host dependence on a sub_list row", status: "owner review", evidence: "measured row recorded in spec Q-1; log lane-probe-q1.log"},
			{q: "Q-2 HTTP reconnect", status: "unchanged, no migration sentence", evidence: "prod.rs:757-770, :811-815"},
			{q: "Q-3 catalogue", status: "measured unchanged", evidence: "make tools-export-check: unchanged twice, exit 0"},
			{q: "Q-4 disposition type", status: "plan decision", evidence: "constraints in spec Q-4"},
			{q: "Q-5 gate totals", status: "2142 lib, 2316 with integration", evidence: "2141 - 5 + 6; 2315 - 5 + 6"},
			{q: "Q-6 tests outside the modules", status: "measured none depend on the lane", evidence: "lib 2141 passed, tests 2315 passed, five suites green"},
			{q: "Q-7 entry/lifecycle window", status: "pre-existing, unchanged, recorded", evidence: "subscribe_resource.rs:159-164; refcount.rs:546-555"},
			{q: "Q-8 rollback against a refresh", status: "rule in FR-004", evidence: "memory_registry.rs:592-596, :977-981"},
			{q: "Q-9 embedded daemon", status: "no change", evidence: "dispatcher.rs:592, :625-627; DAEMON.md:129-160"},
			{q: "Q-10 registry SubId and cursors", status: "untouched, dead surface recorded", evidence: "memory_registry.rs:265-313"},
		]
	}
}
