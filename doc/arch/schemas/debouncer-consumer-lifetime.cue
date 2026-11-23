// DDD role: ValueObject

package schemas

// #StateToday is the defect: the peer list alone decides the stop, and no
// close path reaches the task. Every line was read from the file.
// DDD role: ValueObject
#StateToday: {
	peer_stop: {
		call:      "src/adapters/subscription/memory_registry.rs:459-467"
		function:  "src/adapters/subscription/memory_registry.rs:440-476"
		callers:   ["src/adapters/subscription/memory_registry.rs:428", "src/adapters/subscription/memory_registry.rs:847"]
		no_lane_check: true
	}
	task_stop: {
		function: "src/adapters/subscription/memory_registry.rs:412-420"
		callers:  ["src/adapters/subscription/memory_registry.rs:466"]
		removes:  ["debounce_tasks", "wakers", "flush_now", "bytes_since_flush"]
	}
	task_start: {
		function: "src/adapters/subscription/memory_registry.rs:371-410"
		callers:  ["src/adapters/subscription/memory_registry.rs:499", "src/adapters/subscription/memory_registry.rs:655", "src/adapters/subscription/memory_registry.rs:841"]
		strong_arc: ["src/adapters/subscription/memory_registry.rs:376-380", "src/adapters/subscription/memory_registry.rs:401-408"]
		counter_reuse: "src/adapters/subscription/memory_registry.rs:387-392"
	}
	activator: {
		trait: "src/ports/notifier.rs:86-90"
		impl:  "src/adapters/subscription/memory_registry.rs:651-657"
		call:  "src/application/subscription_admin.rs:162"
	}
	lane_close: {
		trait:   "src/ports/subscriber_lane.rs:126"
		async_impl: "src/adapters/subscription/subscriber_lane.rs:824-833"
		callers: ["src/application/subscription_admin.rs:286", "src/application/disconnect_session.rs:186", "src/application/disconnect_agent.rs:187"]
		removal: "src/adapters/subscription/subscriber_lane.rs:560-570"
	}
	reproductions: {
		source: "not in the tree; /var/tmp/ssh-mcp/end-repro.patch"
		log:    "/var/tmp/ssh-mcp/end-repro-run.log"
		tests: [
			"peer_unsubscribe_stops_the_debouncer_the_lane_still_needs",
			"debouncer_task_survives_lane_close_and_the_end_of_its_resource",
			"every_open_and_close_cycle_on_a_distinct_id_leaks_one_task",
			"debouncer_task_keeps_the_registry_alive_after_all_handles_drop",
		]
	}
}

// #Consumers are the two kinds the stop decision must consider.
// DDD role: ValueObject
#Consumers: {
	peer: {
		table:   "src/adapters/subscription/memory_registry.rs:123-124"
		removal: "src/adapters/subscription/memory_registry.rs:440-476"
	}
	lane: {
		tables:  ["src/adapters/subscription/subscriber_lane.rs:315-319"]
		removal: "src/adapters/subscription/subscriber_lane.rs:560-570"
		lookup:  "src/adapters/subscription/subscriber_lane.rs:442-461"
	}
}

// #StopPolicy is FR-001 to FR-003: one predicate, both paths, and the map
// entries a stop owns.
// DDD role: ValueObject
#StopPolicy: {
	rule: "stop only when the peer list of the URI is empty and no lane is bound to it"
	one_implementation: "src/adapters/subscription/memory_registry.rs:412-420"
	removes: ["debounce_tasks", "wakers", "flush_now", "bytes_since_flush"]
	kept_case: "last peer unsubscribe with no lane still stops"
	fallback: {
		installed_by: "src/composition/prod.rs:337-342"
		absence_rule: "explicit ensure keeps the task; a peer unsubscribe stops it"
		proving_tests: ["ensure_without_a_presence_query_keeps_the_task", "peer_unsubscribe_without_a_presence_query_stops_the_task"]
	}
	maps_stay: {
		kept: ["sequence_counters", "event_tails"]
		readers: ["src/application/read_resource.rs:595", "src/application/read_resource.rs:632", "src/application/read_resource.rs:673", "src/application/read_resource.rs:716"]
		proving_test: "per_resource_maps_keep_the_sequence_and_tail_after_a_stop"
	}
	bytes_recovery: {
		rule: "bytes accumulate with no task; the next wakeup delivers one URI notification"
		ring: "src/adapters/subscription/memory_registry.rs:729-746"
		proving_test: "bytes_written_before_a_debouncer_exists_are_delivered_by_the_next_wakeup"
	}
}

// #Races is FR-004: the three interleavings and the rule that makes each
// safe.
// DDD role: ValueObject
#Races: {
	open_then_ensure: {
		order: ["lane insert", "ensure_for_uri", "src/application/subscription_admin.rs:157-163"]
		rule:  "post-stop re-read of both kinds, then re-ensure if a consumer appeared"
	}
	resources_subscribe: {
		order: ["peer insert", "ensure", "src/adapters/subscription/memory_registry.rs:822-844"]
		rule:  "same post-stop re-read"
	}
	peer_unsubscribe: {
		order: ["peer removal", "stop decision"]
		rule:  "same post-stop re-read"
	}
	primitives: ["DashMap", "atomics", "Notify", "mpsc"]
	no_mutex: "doc/arch/memory/constitution.md:93-96"
	atomicity: {
		rule: "ensure and stop are atomic with respect to each other and to themselves"
		measured: "Q-10a: 10 spawns for 32 concurrent ensures on the base tree, 1 with an entry guard; Q-10b: task/waker split caught within 5,000 base rounds"
		ensure: "src/adapters/subscription/memory_registry.rs:371-410"
	}
	proving_tests: [
		"open_racing_last_lane_close_keeps_one_debouncer",
		"resources_subscribe_racing_last_lane_close_keeps_one_debouncer",
		"peer_unsubscribe_racing_sub_open_keeps_one_debouncer",
	]
}

// #Release is FR-005: the weak side and the measured cycle.
// DDD role: ValueObject
#Release: {
	weak_side: "the sink holds Weak<MemoryRegistry>"
	cycle_measurement: {
		doc:  "doc/arch/sdd/005-subscription-pause-delivery/research.md:46-70"
		test: "src/adapters/subscription/lane_bridge.rs:1244-1259"
		design_a: "adapter-owned strong Arc<dyn LaneResumeSink> retains the adapter"
	}
	proving_test: "registry_is_released_after_the_last_consumer_leaves"
	sequence: "open one lane, ensure, drop every external handle except the adapter, close the lane with no peer, drop the adapter, await Weak::upgrade None for at most 2 s"
}

// #Wiring is FR-006: composition installs the link beside the mux sinks.
// DDD role: ValueObject
#Wiring: {
	build_site:  "src/composition/prod.rs:333-366"
	close_sink:  "src/adapters/subscription/subscriber_lane.rs:404-414"
	sink_type:   "src/adapters/subscription/subscriber_lane.rs:366"
	proving_test: "crate::composition::prod::tests::production_wiring_stops_the_debouncer_when_the_last_lane_closes"
	observable:  "a #[cfg(test)] accessor returns the registry and adapter that build_use_cases builds"
	why:         "a hand-built adapter installs nothing; production must not keep the defect"
	one_composition_test_per_binary: "src/composition/prod.rs:544"
}

// #NoWire is FR-007: nothing on the published surface moves.
// DDD role: ValueObject
#NoWire: {
	catalogue_unchanged: true
	check: "make tools-export-check"
	scenario_tag: "@security"
}

// #Proof is FR-008 to FR-011: the gate floors, the validation baseline and
// the code rules.
// DDD role: ValueObject
#Proof: {
	tests: [
		{name: "peer_unsubscribe_keeps_the_debouncer_a_lane_still_needs", module: "crate::adapters::subscription::memory_registry::tests", fr: "FR-001, FR-002"},
		{name: "last_lane_close_stops_the_debouncer", module: "crate::adapters::subscription::memory_registry::tests", fr: "FR-002"},
		{name: "ensure_after_the_last_consumer_left_spawns_nothing", module: "crate::adapters::subscription::memory_registry::tests", fr: "FR-002"},
		{name: "every_open_and_close_cycle_leaves_no_task", module: "crate::adapters::subscription::memory_registry::tests", fr: "FR-002"},
		{name: "last_peer_unsubscribe_without_a_lane_stops_the_debouncer", module: "crate::adapters::subscription::memory_registry::tests", fr: "FR-003"},
		{name: "open_racing_last_lane_close_keeps_one_debouncer", module: "crate::adapters::subscription::memory_registry::tests", fr: "FR-004"},
		{name: "resources_subscribe_racing_last_lane_close_keeps_one_debouncer", module: "crate::adapters::subscription::memory_registry::tests", fr: "FR-004"},
		{name: "peer_unsubscribe_racing_sub_open_keeps_one_debouncer", module: "crate::adapters::subscription::memory_registry::tests", fr: "FR-004"},
		{name: "concurrent_ensure_spawns_one_task", module: "crate::adapters::subscription::memory_registry::tests", fr: "FR-004"},
		{name: "ensure_racing_stop_never_splits_the_task_from_its_waker", module: "crate::adapters::subscription::memory_registry::tests", fr: "FR-004"},
		{name: "a_stop_that_holds_the_entry_makes_the_opener_rebuild_every_map", module: "crate::adapters::subscription::memory_registry::tests", fr: "FR-004"},
		{name: "registry_is_released_after_the_last_consumer_leaves", module: "crate::adapters::subscription::memory_registry::tests", fr: "FR-005"},
		{name: "session_cascade_stops_child_debouncers_and_spares_the_health_one", module: "crate::application::disconnect_session::tests", fr: "FR-002, Q-12"},
		{name: "agent_cascade_stops_child_debouncers_and_spares_the_health_one", module: "crate::application::disconnect_agent::tests", fr: "FR-002, Q-12"},
		{name: "production_wiring_stops_the_debouncer_when_the_last_lane_closes", module: "crate::composition::prod::tests", fr: "FR-006"},
		{name: "ensure_without_a_presence_query_keeps_the_task", module: "crate::adapters::subscription::memory_registry::tests", fr: "FR-012"},
		{name: "peer_unsubscribe_without_a_presence_query_stops_the_task", module: "crate::adapters::subscription::memory_registry::tests", fr: "FR-012"},
		{name: "per_resource_maps_keep_the_sequence_and_tail_after_a_stop", module: "crate::adapters::subscription::memory_registry::tests", fr: "FR-013"},
		{name: "bytes_written_before_a_debouncer_exists_are_delivered_by_the_next_wakeup", module: "crate::adapters::subscription::memory_registry::tests", fr: "FR-014"},
		{name: "forward_tail_survives_without_a_task", module: "crate::adapters::subscription::memory_registry::tests", fr: "FR-014"},
	]
	floors: {
		lib_tests:         2174
		integration_tests: 2349
		scripts: {
			"scripts/test_v5_adversarial.py": 63
			"scripts/test_v51_flush_bytes.py": 1
		}
	}
	baseline: {
		exit_status: 6
		errors:      68
		warnings:    0
		rule_counts: {
			"fqcn.relative-super":            46
			"branch.else":                    6
			"pin.ci.unpinned-action":         5
			"pin.docker.unpinned-image":      2
			"pin.cargo.lockfile-missing":     1
			"readme.mermaid.missing":         1
			"readme.index.missing":           1
			"readme.image.missing":           1
			"readme.arch-link.missing":       1
			"llms-txt.file.missing":          1
			"fqcn.glob-import":               1
			"crew.roster.dangling":           1
			"ci.gitlab.missing":              1
		}
		no_finding_on: [
			"doc/arch/sdd/008-debouncer-consumer-lifetime/",
			"doc/arch/adr/0008-debouncer-consumer-lifetime.md",
			"doc/arch/schemas/debouncer-consumer-lifetime.cue",
			"doc/arch/specs/features/debouncer-consumer-lifetime.feature",
			"session.md",
		]
	}
	rules: {
		one_removal_implementation:           "remove_debouncer"
		no_new_mutex:                          true
		panic_free:                            "doc/arch/memory/constitution.md:97-101"
		no_super_import:                       true
		no_else_after_early_return:            true
		no_feature_or_fr_in_comments:          true
	}
}

// #SessionEntries is FR-010: the defects this measurement adds or extends in
// session.md. `section` names an entry that does not exist yet.
// DDD role: ValueObject
#SessionEntries: {
	add: [
		{defect: "a peer unsubscribe stops a debouncer a lane still needs", lines: ["src/adapters/subscription/memory_registry.rs:440-476", "src/adapters/subscription/memory_registry.rs:466"], test: "peer_unsubscribe_stops_the_debouncer_the_lane_still_needs", source: "not in the tree; /var/tmp/ssh-mcp/end-repro.patch", pins: "has_debouncer false and no flush after the peer left"},
		{defect: "the debouncer task keeps the registry alive after its composition is dropped", lines: ["src/adapters/subscription/memory_registry.rs:376-380", "src/adapters/subscription/memory_registry.rs:401-408", "src/composition/prod.rs:762-774"], test: "debouncer_task_keeps_the_registry_alive_after_all_handles_drop", source: "not in the tree; /var/tmp/ssh-mcp/end-repro.patch", pins: "Weak::upgrade is Some with every external handle dropped"},
		{defect: "a remote shell EOF leaves the repository row Running and publishes only the local tail", lines: ["src/adapters/ssh/russh_adapter.rs:1498-1527", "src/adapters/ssh/russh_adapter.rs:1566"], source: "no test"},
		{defect: "rsync terminal states publish no notification", lines: ["src/application/rsync_sync.rs:754-778", "src/application/rsync_sync.rs:795-813"], source: "no test"},
		{defect: "serial_close and its frozen description promise a cascade and a final event", lines: ["src/infra/mcp/tool_router.rs:2601", "src/adapters/serial/state.rs:399-406", "src/adapters/serial/state.rs:573-629"], source: "no test"},
		{defect: "close_forward has no caller", lines: ["src/adapters/ssh/russh_adapter.rs:890-897", "src/adapters/ssh/russh_adapter.rs:437-450", "src/adapters/ssh/russh_adapter.rs:695"], source: "no test"},
		{defect: "close_lanes_for_resource is duplicated and scans the whole lane list", lines: ["src/application/disconnect_session.rs:175-188", "src/application/disconnect_agent.rs:176-189", "src/adapters/subscription/subscriber_lane.rs:713-720"], test: "close_lanes_for_resource_scans_the_whole_lane_list", source: "not in the tree; /var/tmp/ssh-mcp/end-repro.patch", pins: "one list() call and 1024 summaries to close one lane"},
		{defect: "sub_close is documented as answering SUB_CLOSE: NOT_FOUND while the code answers an error result", lines: ["docs/API.md:1122", "src/infra/mcp/tool_router.rs:1380", "src/infra/mcp/tool_router.rs:878-882"], source: "no test"},
		{defect: "the last notification is not flushed before the terminal write", lines: ["src/adapters/ssh/internal/client.rs:1144-1150", "src/adapters/ssh/russh_adapter.rs:1511-1526"], tests: ["command_completion_does_not_flush_pending_bytes_before_the_terminal_write", "shell_close_does_not_flush_pending_bytes_before_the_terminal_write"], source: "not in the tree; /var/tmp/ssh-mcp/end-repro.patch", pins: "zero notifications at the terminal write, one about a debounce later"},
		{defect: "concurrent ensure_for_uri calls spawn several tasks for one key", lines: ["src/adapters/subscription/memory_registry.rs:371-410"], test: "concurrent_ensure_spawns_more_than_one_task", source: "not in the tree; /var/tmp/ssh-mcp/end-repro.patch", pins: "10 spawns for 32 concurrent ensures, one reachable handle"},
		{defect: "the guard/open window still yields a lane", lines: ["src/application/subscription_admin.rs:150-160"], test: "a_resource_removed_between_guard_and_open_still_gets_a_lane", source: "not in the tree; /var/tmp/ssh-mcp/end-repro.patch", pins: "one lane on a row removed inside the guard"},
	]
	extend: [
		{entry: "the debouncer nothing stops", lines: ["src/application/subscription_admin.rs:161-163", "src/adapters/subscription/memory_registry.rs:651-657"]},
		{entry: "lanes outlive the shell, the command and the peer", lines: ["src/application/close_shell.rs:102-114", "src/application/execute_command.rs:105-115", "src/adapters/subscription/subscriber_lane.rs:205-208"]},
	]
}

// #DebouncerConsumerLifetime binds the defect, the consumers, the policy, the
// races, the release, the wiring and the proof.
// DDD role: ValueObject
#DebouncerConsumerLifetime: {
	behavior: {
		state:     #StateToday
		consumers: #Consumers
		policy:    #StopPolicy
		races:     #Races
		release:   #Release
	}
	rules: {
		wiring:  #Wiring
		no_wire: #NoWire
	}
	proof:         #Proof
	session:       #SessionEntries
	provenance: "split by the coordinator; mechanism registered in ADR 0008 for the owner's review"
	clarifications: {
		open: [
			{q: "Q-1 every production caller of the lane close and of stop_debouncer", status: "answered; unsubscribe_resource:81 and subscribe_resource:149; no daemon or reaper by another name"},
			{q: "Q-2 whether a stopped debouncer loses bytes a later consumer needs", status: "answered; counter accumulates, next wakeup delivers one URI, ring is the read path"},
			{q: "Q-3 what the daemon and each HTTP session observe", status: "answered; daemon one process composition, HTTP per session with no close path"},
			{q: "Q-4 which existing tests and Python suites depend on a task outliving its consumers", status: "answered; no existing case changes; probe tests only"},
			{q: "Q-5 exact expected test totals", status: "answered; 20 lib, floors 2174 and 2349"},
			{q: "Q-6 where the link lives", status: "plan decision; constraints recorded"},
			{q: "Q-7 whether CloseSink grows kind/resource_id", status: "plan decision; signatures recorded"},
			{q: "Q-8 what happens to sequence_counters and event_tails", status: "answered; both grow per id and stay out of the stop; recorded in session.md"},
			{q: "Q-9 whether a lane count is needed and what resets it", status: "plan decision; increment/decrement rules recorded"},
			{q: "Q-10 whether ensure and stop are atomic", status: "answered; Q-10a and Q-10b defects in scope; entry guard and removal order repair both"},
			{q: "Q-13 what a registry without a presence query does", status: "answered; peer-list semantics, FR-012"},
			{q: "Q-11 how the production wiring is proven", status: "answered; cfg(test) accessor and prod.rs test module"},
			{q: "Q-12 the two cascades", status: "answered; both stop child debouncers, health debouncer stays"},
		]
	}
}
