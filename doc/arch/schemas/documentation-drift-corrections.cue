// DDD role: ValueObject

package schemas

// #DocumentLine is one corrected line: the document, its line of 2026-11-22,
// the false sentence and the verdict.
// DDD role: ValueObject
#DocumentLine: {
	document:       string & !=""
	line:           int & >0
	false_sentence: string & !=""
	verdict:        "false"
}

// #LockCorrection is FR-001: the stale-lock instruction the measurement
// refutes.
// DDD role: ValueObject
#LockCorrection: #DocumentLine & {
	document:       "docs/MIGRATION.md"
	line:           882
	false_sentence: "A stale Cargo.lock must be re-resolved (cargo update -p russh -p russh-sftp) before it builds against the new lines."
	verdict:        "false"
	measurement: {
		directory:   "/var/tmp/ssh-mcp/lockcheck3"
		command:     "cargo metadata --format-version 1"
		exit_status: 0
		locked_before: {russh: "0.60.3", "russh-sftp": "2.4.0", rmcp: "1.8.0"}
		locked_after: {russh: "0.65.4", "russh-sftp": "4.0.0", rmcp: "3.11.0"}
		identical_to_repo: true
	}
}

// #InternalErrorCorrection is FR-002: the claim that the error is never
// raised, and the two emitters that reach it.
// DDD role: ValueObject
#InternalErrorCorrection: #DocumentLine & {
	document:       "docs/OPERATIONS.md"
	line:           409
	false_sentence: "INTERNAL_ERROR for registry-level failures (currently never raised — subscribe is infallible at runtime)."
	verdict:        "false"
	emitters: {
		mapping: "src/infra/mcp/resource_handlers.rs:72-79"
		cap:     "src/infra/mcp/resource_handlers.rs:130-131"
	}
	reachable_by: "the 17th resources/subscribe from one peer on one URI at SSH_MAX_SUBS_PER_URI=16"
}

// #CodeName is one documentation occurrence of a code that does not exist.
// DDD role: ValueObject
#CodeName: {
	document: string & !=""
	line:     int & >0
	written:  string & !=""
	emitted:  string & !=""
}

// #FilterCorrection is FR-004: the three wrong occurrences of FILTER_INVALID;
// the CONFIGURATION promise stays because the cap is a code defect.
// DDD role: ValueObject
#FilterCorrection: {
	written: "FILTER_INVALID"
	emitted: "INVALID_ARGUMENT"
	occurrences: {
		api_open: #CodeName & {document: "docs/API.md", line: 1106, written: "FILTER_INVALID", emitted: "INVALID_ARGUMENT"}
		api_filter: #CodeName & {document: "docs/API.md", line: 1153, written: "FILTER_INVALID", emitted: "INVALID_ARGUMENT"}
		threat_model: #CodeName & {document: "doc/arch/threat-model/threat-model.md", line: 755, written: "FILTER_INVALID", emitted: "INVALID_ARGUMENT"}
	}
	excluded: {
		configuration: #CodeName & {document: "docs/CONFIGURATION.md", line: 153, written: "FILTER_INVALID", emitted: "FILTER_INVALID"}
		reason: "the SSH_FILTER_REGEX_MAX cap is resolved and not applied; a later feature applies it"
	}
	cap_applied:  false
	cap_resolver: "src/adapters/config/internal/mod.rs:906-911"
}

// #SubOpenErrorTable is the complete set of FR-005: code and emitter.
// DDD role: ValueObject
#SubOpenErrorTable: [...{
	code:    string & !=""
	emitter: string & !=""
}] & [
	{code: "INVALID_ARGUMENT", emitter: "src/application/subscription_admin.rs:137; src/adapters/subscription/filter.rs:58; src/infra/mcp/tool_router.rs:495-510"},
	{code: "RESOURCE_GONE", emitter: "src/adapters/lifecycle/refcount.rs:420"},
	{code: "MAX_SUBS_PER_URI_EXCEEDED", emitter: "src/adapters/subscription/subscriber_lane.rs:531"},
	{code: "MAX_SUBS_TOTAL_EXCEEDED", emitter: "src/adapters/subscription/subscriber_lane.rs:470"},
]

// #DaemonCorrection is FR-006: the op emits one code, not a list.
// DDD role: ValueObject
#DaemonCorrection: #DocumentLine & {
	document:       "docs/DAEMON.md"
	line:           147
	false_sentence: "Errors: SUB_NOT_FOUND (parent resource gone), RESOURCE_GONE, MAX_SUBS_PER_URI_EXCEEDED, MAX_SUBS_TOTAL_EXCEEDED, INVALID_LIFETIME, INVALID_LAG_POLICY."
	verdict:        "false"
	emitted:        "SUBSCRIBE_FAILED"
	emitter:        "src/embed/dispatcher.rs:596-601"
	detail: {
		shape:   "Mcp error: <json-rpc code>: <domain display>"
		capture: "{\"ev\":\"err\",\"id\":\"cap-17\",\"code\":\"SUBSCRIBE_FAILED\",\"reason\":\"subscribe forward://x/events failed\",\"detail\":\"Mcp error: -32603: max subs per URI exceeded for forward://x/events (limit 16)\"}"
	}
}

// #Baseline is the validate baseline FR-008 must not grow.
// DDD role: ValueObject
#Baseline: {
	exit:     6
	errors:   70
	warnings: 0
	per_rule: {
		"fqcn.relative-super":        46
		"branch.else":                8
		"pin.ci.unpinned-action":     5
		"pin.docker.unpinned-image":  2
		"readme.mermaid.missing":     1
		"readme.index.missing":       1
		"readme.image.missing":       1
		"readme.arch-link.missing":   1
		"pin.cargo.lockfile-missing": 1
		"llms-txt.file.missing":      1
		"fqcn.glob-import":           1
		"crew.roster.dangling":       1
		"ci.gitlab.missing":          1
	}
}

// #TouchedDocuments is the complete allow-list of FR-007.
// DDD role: ValueObject
#TouchedDocuments: [...string] & [
	"docs/MIGRATION.md",
	"docs/OPERATIONS.md",
	"docs/CONFIGURATION.md",
	"docs/API.md",
	"docs/DAEMON.md",
	"doc/arch/threat-model/threat-model.md",
	"session.md",
]

// #DocumentationDriftCorrections binds the three candidates, the code table,
// the baseline and the constraints of feature 004.
// DDD role: ValueObject
#DocumentationDriftCorrections: {
	corrections: {
		lock:           #LockCorrection
		internal_error: #InternalErrorCorrection
		cap_names: {
			per_uri: {document: "docs/CONFIGURATION.md", line: 149, written: "LANE_LIMIT_PER_URI", emitted: "MAX_SUBS_PER_URI_EXCEEDED"}
			total: {document: "docs/CONFIGURATION.md", line: 150, written: "LANE_LIMIT_TOTAL", emitted: "MAX_SUBS_TOTAL_EXCEEDED"}
		}
		filter: #FilterCorrection
		daemon: #DaemonCorrection
	}
	sub_open_errors: #SubOpenErrorTable
	baseline:        #Baseline
	constraints: {
		touched:              #TouchedDocuments
		no_code_change:       true
		tools_export:         "unchanged"
		defects_recorded:     3
		owner_items_recorded: 1
	}
}
