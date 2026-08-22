# Architecture decisions

Record each concrete project decision before or with its implementation.

Update an existing record when the decision has the same owner and boundary.
Create a new record when the owner, dependency direction, failure policy, or
program order changes.

Each record states:

- The chosen behavior.
- The authoritative owner.
- The invariants.
- The failure and recovery behavior.
- The rejected alternatives that a future agent could reasonably retry.
- The migration consequences.
- The proof required before the decision is complete.

`docs/architecture.md` describes the current system.
These records explain durable choices and their reasons.
The vault context owns product vocabulary.
Code and tests own implementation detail.

Do not copy one source into another.
Link the authoritative source.
