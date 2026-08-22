# Architecture work precedes integrations

Status: accepted on 2026-08-13.

Implementation: in progress.

Bootty will establish its architecture before it adds agent integrations.
The work order is architecture foundations, deep module and code simplification,
one CLI/socket/Luau command path, the generic extension lifecycle, and agent
integrations through the approved local extension boundary.

## Invariants

- Each stage builds on the authority and interfaces from the prior stage.
- One stage can contain many vertical slices and commits.
- A stage completes only when its ownership and failure rules exist in production
  code and have fast public behavior proof.
- A test move, file move, deleted-line count, module count, or `AppState` size is
  not a stage outcome.
- Agent-specific schemas, install rules, and lifecycle policy stay outside Rust.
- This roadmap does not decide exact installation or asset placement.

## Rejected alternatives

- Building agent adapters before the host contracts are stable would encode
  agent policy in unfinished Rust seams.
- Building an extension marketplace, registry, permission matrix, or remote
  distribution system would solve no approved user flow.
- Using a line-count target would reward churn and shallow wrappers.

## Consequences

Current work can change broad architecture and fix discovered bugs.
It does not add agent features.
Later control, extension, and agent work must use the owners documented in
`docs/architecture.md`.

## Proof

Each completed slice updates the current architecture description. Add or
supersede a decision record only when the decision changes. Production slices
pass focused public contracts and the applicable repository correctness gate.
Documentation-only decision slices require source-grounded review.
