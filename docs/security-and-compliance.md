# Security and compliance

How this tool handles secrets, personal data, audit and logs — and how that maps onto the
FGV *Norma de Aquisição, Desenvolvimento e Manutenção de Sistemas* (NRM v2, April 2021) and
guide **G-002**.

The norm is binding and carries sanctions from the CSI. Its rules are treated here as
requirements, not recommendations.

---

## 1. Classification — **proposal, pending ESI validation**

Data processed: names, corporate e-mails and organisational units of employees; serial
numbers of security tokens; and the map of which credential material sits on which token.

**Proposed level: 3.**

The personal data alone would suggest level 2. What raises it is the token↔person map: it is
precisely the reconnaissance an attacker wants before targeting authentication, which makes
it strategically sensitive to FGV under NRM §2's broader definition of sensitive data.

Level 3 brings: a change request for every version installed anywhere, known security
defects taking priority over feature work, and a prohibition on discontinued or unsupported
components.

If secrets are ever escrowed in this tool, the level should be revisited upward — which is
one more reason not to escrow them here
([`../features/secrets-custody.md`](../features/secrets-custody.md)).

**The ESI validates the level.** This document proposes; it does not decide.

---

## 2. Secrets

### The rule

No PIN, PUK, management key, OTP access code or database password reaches a log, an audit
entry, a database column, an error message, a UI label, a window title, a temporary file or
a panic message.

### How the code enforces it

- Secrets in a plan are `template::Arg::Secret("FIDO2-PIN")` — a **label**, not a value.
  `redacted()` renders `<FIDO2-PIN>`. There is no field in `app`, `ui`, `domain` or `store`
  that holds a secret, so no code path can persist one.
- `BootstrapRun.custody` records *where* custody went (`forced-change`,
  `envelope:2026-08-10-014`, `bastionvault:kv/yubikeys/20423633`), never a value.
- `no_plan_output_can_leak_a_secret` asserts that no rendered plan contains a
  secret-looking literal — including the published factory defaults, so nobody can pre-fill
  them into a template.
- Command construction uses argv vectors, never a shell string; the native transport passes
  secrets as function parameters instead of command-line arguments.

### When real secret input arrives (Wave 1)

Rules already fixed: OS CSPRNG only; in memory for the shortest possible time; zeroised
after use (`zeroize`); a manual `Debug` that prints `<redacted>`; shown once in a panel the
operator dismisses deliberately; never copied to the clipboard silently.

NRM §5.3.2: data that does not need recovery is hashed one-way; reversible data is opened
only in memory, for the shortest possible time. Here the cleanest reading is that most of
these secrets need *neither* — the holder sets them, or the key generates and keeps them.

### No secret in the repository

Not in code, configuration, tests, fixtures or Git history. A secret that ever reached a
commit is an **incident** (NRM §5.4.4): rotate it and escalate to the ESI. Removing the file
is not a fix.

---

## 3. Personal data (LGPD)

### What is held, and why

| Field | Purpose | Table |
|---|---|---|
| Full name | Certificate `CN`; the name on the hand-over term | `holders`, `distributions` |
| Corporate e-mail | Certificate `rfc822Name` SAN; identifies the holder | `holders`, `distributions` |
| Unit | Certificate `OU`; who to contact about a key | `holders` |
| Registration | Asset control, where the unit uses it | `holders` (optional) |
| Operator name | Accountability for a hand-over and every audit entry | `distributions`, `bootstrap_runs`, `audit` |

No phone, address, ID document, photo, or any special-category data. The full inventory is
in [data-model.md](data-model.md) §Personal data summary, which is also the input to the
FGV data documentation artefact.

### Rules

- Adding a field means adding a purpose and updating that inventory in the same commit.
- A **new category** of personal data needs the DPO's assessment. That is not the
  implementer's call.
- Every input is length-bounded (`domain::MAX_TEXT`, `MAX_NOTE`) per NRM §5.3.5.
- Exports contain personal data and leave the application's protection: every export is
  audited, and the operator is told.
- Production data never goes to a development or test environment without masking
  (NRM §5.3.2). Test data in this repository is fictional (`Ana Silva`, `Bruno Costa`); the
  one real value that appears is a serial number of a test key, with no person attached.

---

## 4. Audit

NRM §5.3.1 requires: login, account creation and account changes **always** audited; audit
stored in a different instance from operational data; nobody able to delete or alter audit
records, **guaranteed by database restrictions**; and inserts kept cheap.

| Requirement | Status |
|---|---|
| Immutability by database restriction | **Met** — `BEFORE UPDATE`/`BEFORE DELETE` triggers `RAISE(ABORT)` on the `audit` table |
| Tamper evidence | **Exceeded** — SHA-256 chain over every entry, verifiable from the GUI |
| Cheap inserts | **Met** — one `INSERT`, single index (the primary key), no triggers on the insert path |
| Audit never silently fails | **Met** — logged at `error` and shown as `AUDIT FAILURE:` in the status bar |
| Separate instance | **Gap** — see below |
| Login / account events audited | **Gap** — there is no operator authentication yet |
| Mechanisms documented | **Met** — [`../features/audit-trail.md`](../features/audit-trail.md) |

### Declared gap 1 — segregation

The requirement is a single file that can live on a share; the norm wants audit data in a
separate instance. Current design: same file, immutable by trigger, plus an optional
append-only mirror on separate storage
([`../features/audit-trail.md`](../features/audit-trail.md) Phase 2). **This needs ESI
sign-off**; it is not a decision to make quietly.

### Declared gap 2 — operator identity

`app.operator` comes from `$USER` and is editable. It is a label, not authentication, so
today's `actor` field is only as strong as physical control of the workstation.
[`../features/operator-auth-and-roles.md`](../features/operator-auth-and-roles.md) closes
it; until then the gap is declared rather than glossed over.

### Retention

The norm does not fix a period for a system in operation. Nothing is deleted until the ESI
decides. Do not invent a period.

---

## 5. Logs

G-002 fixes the format and the levels; both are implemented in
[`src/logging.rs`](../src/logging.rs).

| Requirement | Status |
|---|---|
| One logging library/rule across the application | Met — `logging::init`, custom formatter, no hand-built lines |
| At least three levels | Met — Informação / Aviso / Erro |
| Format `[dd/mm/aaaa] hh:mm:ss ; evento ; detalhes` | Met |
| Every error logged, no swallowed exception | Met by rule and review; `Result` is never discarded |
| Errors to the log, not to the screen alone | Met — errors go to both, deliberately: the operator needs to see the refusal |
| Never log a secret | Met by design (no secret exists in a loggable field) |
| File sink | **Gap** — output goes to stderr, which a GUI user never sees ([`../features/logging.md`](../features/logging.md) Phase 2) |

---

## 6. Code

| Rule (NRM §5.3.5) | Status |
|---|---|
| Parameterised queries always | Met — every statement uses bound parameters; identifiers are source literals |
| Maximum length on every input | Met — `MAX_TEXT` / `MAX_NOTE`, enforced in the domain and in the widgets |
| Output escaping | N/A for a native GUI (no HTML); RFC 4514 escaping is implemented for certificate subjects |
| Errors handled and logged, never shown raw | Met |
| No global variable fed by user input | Met |
| No diagnostic shortcut that queries the database directly | Met — no SQL console, no debug screen |
| No discontinued or unsupported components | Met at the time of writing; dependency review is part of the release process |

Beyond the norm, because this tool writes to security hardware:

- Nothing mutates hardware as a side effect of opening a screen.
- Destructive operations name what will be lost before they run.
- A refusal is explained ("illegal status transition: In stock -> Distributed"), never
  silent.

---

## 7. Approval gates — not the implementer's to grant

| Gate | Owner |
|---|---|
| Architecture security premises, and any change to them | **ESI** |
| Security verification before production, every version | **ESI** |
| Every integration mechanism (AD, CA, BastionVault) | **ESI** |
| Cipher and KDF parameters for the encrypted database | **ESI** |
| Audit and log retention | **ESI** |
| Classification level | **ESI** |
| Privacy notice, lawful basis, consent | **DPO** |
| Assessment of a system processing personal data under FGV control | **DCI** |
| Adequacy plan for a declared gap | **CSI** (ESI first) |

When work is blocked on one of these: write the assumption in the feature file, build
everything that does not depend on it, and say plainly what is pending. That is what the
*Open questions* section of [`../roadmap.md`](../roadmap.md) is for.

---

## 8. Declared gaps, consolidated

1. **Audit segregation** — one file with trigger-enforced immutability instead of a separate
   instance. Mirror designed, not built. *ESI sign-off required.*
2. **Operator authentication** — none; `$USER` is a label. *Feature specified.*
3. **AD integration** — required by the norm, not built. *Feature specified.*
4. **Log file sink** — stderr only today.
5. **G-002 v2.0 (July 2026)** — could carry more specific requirements (OWASP ASVS, NIST
   SSDF, DevSecOps). The copy available when this was written was IRM-protected and could not
   be read. **Ask the ESI for the current text before homologation**; where it conflicts with
   this document, the official document prevails.
6. **Custody model undecided** — until it is, the tool does not write to keys at all, which
   is the conservative failure mode.

Declaring gaps with an adequacy plan is the process the norm anticipates (§5.4.6). A claim of
full conformance that does not survive a look at the code would not be.

---

## References

- NRM v2 (April 2021); G-002 v1.0 (June 2013)
- [`../AGENTS.md`](../AGENTS.md) — the same rules as day-to-day engineering practice
- [`../features/fgv-compliance.md`](../features/fgv-compliance.md) — the artefacts to produce
- [`../features/secrets-custody.md`](../features/secrets-custody.md),
  [`../features/audit-trail.md`](../features/audit-trail.md),
  [`../features/db-password-and-encryption.md`](../features/db-password-and-encryption.md)
