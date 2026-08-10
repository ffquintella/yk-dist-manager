# Documentation

| Document | Read it when |
|---|---|
| [architecture.md](architecture.md) | You need to know where something belongs, or why the module boundaries are where they are |
| [data-model.md](data-model.md) | You are touching the schema, or need to know what a column means |
| [bootstrap-procedure.md](bootstrap-procedure.md) | You want the procedure itself: every step, in order, with the real commands and the traps |
| [yubikey-reference.md](yubikey-reference.md) | You need to know what the hardware can do, natively vs through `ykman`, and which firmware gates apply |
| [security-and-compliance.md](security-and-compliance.md) | You are handling secrets, personal data, audit or logs — or preparing for a review |
| [operations.md](operations.md) | You are running the tool: distribute a key, handle a return, a loss, a backup, a share |
| [gui.md](gui.md) | You are adding or changing a screen |
| [development.md](development.md) | You are setting up, or adding a bootstrap step, a record type or a migration |

Planning lives outside this folder:

- [`../roadmap.md`](../roadmap.md) — waves, status, open questions, decision log
- [`../features/`](../features/) — one spec per feature, with phases, audit events and tests
- [`../AGENTS.md`](../AGENTS.md) — the binding working agreement
- [`../CHANGELOG.md`](../CHANGELOG.md) — what shipped

## Keeping this in sync

Docs go stale silently, so the rule from `AGENTS.md` applies: a commit that changes
behaviour, schema or procedure updates the affected document in the same commit. Where a
document states something the code enforces (the template variable list, the ykman command
surface, the audit event set), there is a test asserting it — see the *Tests* sections in
the feature specs.
