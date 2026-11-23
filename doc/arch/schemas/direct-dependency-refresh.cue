// DDD role: ValueObject

package schemas

// #StringList is a list of names: crates, removed packages or gate commands.
// DDD role: ValueObject
#StringList: [...string]

// #TowerHttpMove is the tower-http move. It removes no duplicate.
// DDD role: ValueObject
#TowerHttpMove: {
	crate:             "tower-http"
	manifest_line:     89
	from:              "0.6"
	to:                "0.7"
	locked_minimum:    "0.7.1"
	removes_duplicate: false
}

// #Base64Move is the base64 move. It removes the 0.22.1 duplicate.
// DDD role: ValueObject
#Base64Move: {
	crate:             "base64"
	manifest_line:     110
	from:              "0.22"
	to:                "0.23"
	locked_minimum:    "0.23.1"
	removes_duplicate: true
}

// #Sha2Move is the sha2 move, one half of the digest pair.
// DDD role: ValueObject
#Sha2Move: {
	crate:             "sha2"
	manifest_line:     129
	from:              "0.10"
	to:                "0.11"
	locked_minimum:    "0.11.1"
	removes_duplicate: true
}

// #Md5Move is the md-5 move, the other half of the digest pair.
// DDD role: ValueObject
#Md5Move: {
	crate:             "md-5"
	manifest_line:     131
	from:              "0.10"
	to:                "0.11"
	locked_minimum:    "0.11.0"
	removes_duplicate: true
}

// #DigestCoupling lists why sha2 and md-5 are one requirement: they are the
// only users of digest 0.10, and only the pair removes these packages.
// DDD role: ValueObject
#DigestCoupling: [
	"sha2 and md-5 are the only users of digest 0.10",
	"digest 0.10.7 leaves",
	"block-buffer 0.10.4 leaves",
	"cpufeatures 0.2.17 leaves",
	"crypto-common 0.1.7 leaves",
	"generic-array 0.14.7 leaves",
	"version_check 0.9.5 leaves",
]

// #FrozenOutputs is the proof state each wire output has once the feature is
// implemented, measured 2026-11-22.
// DDD role: ValueObject
#FrozenOutputs: {
	bytes_b64: {
		producer:         "src/adapters/notifier/rmcp_adapter.rs:119"
		known_answer:     "aGVsbG8="
		pinned_by:        "crate::adapters::notifier::rmcp_adapter::tests::inline_payload_params_shape_matches_adr0012; crate::adapters::notifier::rmcp_adapter::tests::inline_payload_params_known_answers"
		has_known_answer: true
		test_kind:        "test"
	}
	resume_sha256: {
		producer:         "src/adapters/sftp/internal/sftp.rs:185"
		pinned_by:        "crate::adapters::sftp::internal::sftp::tests::verify_helpers::sha256_local_prefix_hashes_only_the_prefix"
		has_known_answer: true
		test_kind:        "tokio-test"
	}
	rsync_md5_trailer: {
		producer:         "src/adapters/rsync/wire/hash.rs:270"
		pinned_by:        "crate::adapters::rsync::wire::hash::tests::file_hasher_known_answers; crate::adapters::rsync::wire::hash::tests::file_hasher_for_protocol_31_uses_md5_no_seed"
		has_known_answer: true
		test_kind:        "test"
	}
	trace_events: {
		producer:                     "src/composition/prod.rs:792"
		level:                        "DEBUG"
		visible_under_default_filter: false
		capture_base_vs_probe:        "streamed-identical; fixed-length-adds-on-eos"
	}
}

// #Gate is the verified-change gate, its two test floors and the
// no-weakening rule.
// DDD role: ValueObject
#Gate: {
	commands: #StringList & [
		"cargo fmt --all -- --check",
		"cargo check --all-features --all-targets",
		"cargo build --bins",
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
	lib_tests_minimum:          2122
	integration_tests_minimum:  2295
	no_lint_disabled:           true
	no_allow_added:             true
	no_test_ignored_or_removed: true
}

// #DirectDependencyRefresh binds the four concrete moves, the coupling, the
// frozen outputs and the gate of feature 003.
// DDD role: ValueObject
#DirectDependencyRefresh: {
	tower_http: #TowerHttpMove
	base64:     #Base64Move
	sha2:       #Sha2Move
	md_5:       #Md5Move
	coupling:   #DigestCoupling
	frozen:     #FrozenOutputs
	gate:       #Gate
}
