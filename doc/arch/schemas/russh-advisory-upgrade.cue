// DDD role: ValueObject

package schemas

// #DependencyMove names one direct dependency of the SSH adapter and the two
// requirement lines this feature moves it between.
// DDD role: ValueObject
#DependencyMove: {
	// Crate name as Cargo.toml declares it.
	crate: string & !=""
	// Requirement the manifest declares before the move.
	from: string & !=""
	// Requirement the manifest declares after the move.
	to: string & !=""
}

// #HostKeyTrustRule states what the client does with the key material a server
// presents during the handshake.
// DDD role: ValueObject
#HostKeyTrustRule: {
	// A certificate is outside the trust decision: rejected, fail closed.
	certificate: "rejected"
	// A plain host key keeps the known_hosts decision.
	plain_key: "known_hosts"
}
