# Security and compliance

How this tool handles secrets, personal data, audit and logs — and how that maps onto the
institutional *system acquisition, development and maintenance norm* (**NRM** v2, April
2021) and its secure-systems guide (**G-002**).

The norm is binding and carries sanctions from the CSI. Its rules are treated here as
requirements, not recommendations.

---

## 1. Classification — **level 2**, set 2026-08-11

Data processed: names, corporate e-mails and organisational units of employees; serial
numbers of security tokens; and the map of which credential material sits on which token.

**Level: 2.** This document originally proposed 3, arguing that the token↔person map is
reconnaissance an attacker wants before targeting authentication. The owner set the level at
2: the personal data is ordinary corporate directory data, and the map is protected by the
controls in this document rather than by the classification.

The obligations that follow the tool at level 2 are unchanged by that decision, because they
were never conditional on the level: no secret persisted anywhere
([§4](#4-secrets)), a hash-chained audit trail that the database itself refuses to rewrite
([§5](#5-audit)), and an encryption option for the file.

**Two things would put the level back in question**, and both should trigger a re-read rather
than a quiet continuation:

- **Escrow.** If this tool ever retains a per-device secret, it becomes a secret store, and
  level 3 with it. Model B exists precisely so it does not
  ([`../features/secrets-custody.md`](../features/secrets-custody.md)).
- **Scale or scope.** A deployment covering a materially larger population, or one that
  starts recording something beyond the fields above, is a different data set from the one
  classified here.

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
- **The one secret that has to reach a subprocess goes down its standard input.** The OTP
  access code is written by `ykman otp settings --new-access-code -`, and the `-` is what
  makes that acceptable: it means *prompt*, and a prompt reads stdin. An argument vector is
  readable by every process on the workstation, which is the same reasoning that made the
  SMB backends native APIs rather than `net use`. `ykman::run_with_stdin` logs the
  **arguments** and never the input, and the arguments hold no secret by construction. The
  value goes in twice because that prompt confirms — read out of `ykman`'s own source, not
  guessed, since a prompt left waiting would hang a run in front of an operator.
- **Custody of the OTP access code is recorded, never the value** (`secret.custody`: the
  step, the slot, `custody=sealed-envelope`, `retained=no`). Under the model the owner
  confirmed on 2026-08-11 the code travels to the holder on the sealed slip, which is what
  keeps a protected slot reprogrammable later without an applet reset.
- **A share password is the one secret this tool does handle, and it is handled as a
  transient.** `store::smb::Secret` keeps it as bytes, zeroes them on drop, prints
  `Secret(********)` from `Debug` — so no `{:?}`, no `tracing` field and no panic message can
  carry it — and is readable only inside the crate, so no test can assert on one and no
  widget can echo one. It is typed at the chooser, used for one connection, and cleared from
  the form in the same call.
  This is also *why* the SMB backends are native APIs (`WNetAddConnection2W`,
  `NetFSMountURLSync`) rather than `net use` and `mount_smbfs`: those take the password in an
  argument vector, which every process on the workstation can read, and the documented
  alternative is a credentials file — the temporary file this section forbids. The settings
  file remembers the share and the user name; it has no field a password could occupy, and a
  test asserts the serialised form.
- **The database password is handled the same way**, and is the other secret this tool
  touches: typed, used for one `PRAGMA key`, and cleared from the form in the same call.
  It is in no setting, no column, no log line and no audit entry — a *failed* unlock is
  recorded as `consecutive_failures=N` and nothing else, not even a length, and a
  successful one as `db.unlocked` with no detail. Changing it never re-keys the file in
  place: the register is exported under the new key, the copy is verified, and only then
  swapped (`../features/db-password-and-encryption.md`). The policy is a **12-character
  floor with advice**, enforced by the store where a password is chosen rather than by
  the screen that suggests it, because the threat this password answers is an offline
  attack on a copied file — where length is what buys time and a composition rule
  reliably produces `Password1!`. The prompt itself is throttled after three wrong
  attempts, and deliberately never locks: there is no administrator to lift a lockout on
  a register a whole unit shares, so one would be a denial of service anybody holding the
  file could trigger.

### Custody — decided

**Model B (2026-08-10): transport secret plus forced change.** The operator sets a
temporary secret, the key is marked so the holder must replace it before first use, and
**this tool retains nothing**. FIDO2 enforces the change in firmware from 5.7
(`forcePINChange`); below that, and for PIV at any firmware level, the change is instructed
on the hand-over term, and the run records which of the two applied.

This is the reading of NRM §5.3.2 that fits: these secrets need neither one-way hashing nor
reversible storage, because none of them is stored at all. The management key is random and
kept on the key itself (PIN-guarded), so it is the one generated secret that never travels.

Consequences to be honest about:

- A transport secret must travel to the holder out of band (in person, or a sealed printed
  envelope). That channel is part of the procedure, not an afterthought.
- Where enforcement is procedural, a transport PIN can survive if the holder ignores the
  instruction. The run records `instructed-on-handover`, so this is auditable rather than
  invisible.
- There is no recovery. A holder who forgets the PIN they set needs a reset and a new
  certificate. That is the accepted cost of retaining nothing.

### When real secret input arrives (Wave 1)

Rules already fixed: OS CSPRNG only; in memory for the shortest possible time; zeroised
after use (`zeroize`); a manual `Debug` that prints `<redacted>`; shown once in a panel the
operator dismisses deliberately; never copied to the clipboard silently.

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
| **Identification number** | Named on the consignment term (CPF or the local equivalent) | `holders` (optional) |
| **Phone**, **address** | Contacting a holder; posting a key | `holders` (optional) |
| Operator name | Accountability for a hand-over and every audit entry | `distributions`, `bootstrap_runs`, `audit` |
| Operator name + workstation name | Who currently has a cloud-hosted database open, so a second operator is refused by name rather than allowed to fork the register | `<database>.lock` (a file, not a table — see [`../features/cloud-sync-hosting.md`](../features/cloud-sync-hosting.md)) |
| **Signed term (document)** | The evidence that a key was signed for | `documents.content` |
| **Loss report** — who reported a key lost or stolen, whose key it was, and the circumstances | The record a possible credential compromise is handled from, and reported to the ESI on (NRM §5.4.4) | `key_incidents` |

Two additions in v0.2.x raise what a copy of this database is worth, and both are
stated rather than glossed over:

1. An **identification number** is a step up in sensitivity from a name and a work
   e-mail. It is optional, and exists because the consignment term names it.
2. A **signed term** is a scanned document carrying a name, an identification number
   and a handwritten signature, stored inside the database (the reasoning is in
   `../features/signed-term-documents.md`).

The **incident note** (`crate::incident`) is the other document this tool produces about a
person, and it is the most concentrated: one page saying who held a key, what credentials
were on it, and which of them are still live. Three decisions follow from that, and each is
deliberate:

* **It is not stored.** The incident, the dependency list and the remediations are already
  rows; filing a rendering of them would be a second copy that can go stale, and — unlike a
  consignment term — nothing signs it, so it is not evidence.
* **It is produced on demand and audited when it is** (`key.incident_note`, with the format),
  because a copy leaving the tool is the moment the personal data does.
* **Its PDF metadata carries no personal data**: `/Title` and `/Subject` name the serial, not
  the holder, for the same reason the term's do not — metadata travels with a file into mail
  clients, previews and search indexes.

A **generated term** — text or PDF — is personal data the operator deliberately writes to
a file, at a path they choose, in order to have it signed. It leaves this tool's
protection at that moment, so it is audited (`term.saved`, with the format and the path)
and it is the operator's to file or shred. In the PDF, the document *metadata* carries no
personal data: `/Title` and `/Subject` name the term, the language and the serial, because
those fields travel with the file into mail clients, previews and search indexes, and the
body already says everything the document needs to say.

The **lock file** a cloud-hosted database needs is the one place personal data is written
outside the database *without the operator asking for it*: an operator name, a workstation
name and a pid, all of which the audit trail already records, and no more. It is deleted when the database is closed. It
holds **no secret** — no password, no PIN, no access code — like every other file this
tool writes. A database in a sync folder is itself an argument for the password: the file
sits in somebody else's storage, and its sharing settings become its access control.

Both are direct arguments for turning the database password on
(`../features/db-password-and-encryption.md`). A unit filing signed terms in an
unencrypted database on an open share should understand what it has built.

No phone, address, ID document, photo, or any special-category data. The full inventory is
in [data-model.md](data-model.md) §Personal data summary, which is also the input to the
organisation's data documentation artefact.

### Rules

- Adding a field means adding a purpose and updating that inventory in the same commit.
- Documents are validated before storage: non-empty, at most 8 MiB, scanner formats
  only, and an uploaded filename is treated as data — any directory component is
  stripped, so a name like `../../etc/passwd.pdf` cannot escape.
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
- **The procedure applied to a key can be signed, and a deployment can refuse to run an
  unsigned one** (`../features/bootstrap-templates.md` phase 5). A template decides what
  is written to security hardware, so an unauthorised edit of one is an attack rather
  than a data-quality problem: the audit trail attributes the change afterwards, and the
  signature is what prevents the changed procedure from being used. Ed25519 over a
  documented canonical encoding; **verification only** — the private key never comes near
  this application, because §2 above forbids it holding a secret that persists, so
  signing is an out-of-band step and the tool exports the exact bytes to sign. The
  trusted public keys are a per-deployment setting. Two gates for the **ESI**: whose key
  it is and what protects it, and whether Ed25519 is the algorithm the organisation
  wants. Until a deployment turns the requirement on it runs in *pilot mode*, which is
  stated on screen and recorded per run as `template.unsigned_used` rather than being
  silent.
- **An unsigned responsibility term is visible, and its age is recorded**
  (`../features/receipts-and-terms.md` phase 4). The term is where the holder acknowledges
  that the key is a credential and that a loss must be reported — the acknowledgement the
  loss procedure rests on — so a hand-over without one is a gap in the record rather than
  a missing attachment. The state is derived from the record and what is filed *per kind*
  (a term the tool generated is not a signed one), the threshold is the unit's, and
  `receipt.pending_overdue` is written once per hand-over using the immutable trail as its
  own marker. A key returned without a signed term stays counted: the gap is permanent and
  the register does not tidy away its own history. The **return receipt** closes the loop
  at the other end, and states that the certificates will be revoked — a returned key
  whose certificate is still valid is a credential in a drawer.
- **A procedure crossing between installations is a file, not retyping** (phase 4).
  Export and import are both audited, with the procedure's fingerprint and — on import —
  the signature verdict and the file it came from. An imported procedure goes through the
  same gate as an edited one, because a file is untrusted input.

---

## 7. Approval gates — not the implementer's to grant

| Gate | Owner |
|---|---|
| Architecture security premises, and any change to them | **ESI** |
| Security verification before production, every version | **ESI** |
| Every integration mechanism (AD, CA, BastionVault) | **ESI** |
| Cipher and KDF parameters for the encrypted database | **ESI** |
| The key that signs a bootstrap procedure, and what protects it | **ESI** |
| Whether Ed25519 is the signature algorithm for procedures | **ESI** |
| Audit and log retention | **ESI** |
| Classification level | **ESI** |
| Privacy notice, lawful basis, consent | **DPO** |
| Assessment of a system processing personal data under the organisation's control | **DCI** |
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
6. **A database in a cloud-sync folder** is serialised by a cooperative lock file, not by
   a lock manager: it binds workstations running this tool, and cannot bind one that does
   not. Sync conflict copies are detected and reported rather than prevented. Whether the
   register may live in third-party sync storage at all, and under what sharing rules, is
   an *ESI* decision (`../features/cloud-sync-hosting.md`).
7. **A named account on an SMB share** is a password typed into a desktop application. It is
   never stored (§2), but the *pattern* may be one the ESI prefers to forbid in favour of the
   signed-in user plus a share ACL — which is why the signed-in user is the default and the
   named account has to be chosen. Which share may hold the register, what its ACL must be,
   and whether guest access is ever acceptable are **ESI** decisions
   (`../features/smb-share-hosting.md`). Reaching a share as the signed-in domain user is
   ordinary file access; *selecting* a domain controller or requesting a Kerberos ticket would
   be an AD integration, and is deliberately not built.
8. **Revocation and credential removal are recorded, not performed.** A lost key's
   certificate is revoked at the CA that issued it and its credentials are removed at the
   relying party that holds them — both somebody else's systems, and the issuer is the
   operator by the decision of 2026-08-13. The register lists what has to be dealt with,
   holds the reason and the reference, and refuses to close an incident over a gap silently;
   it cannot prove the revocation happened. The gap closes when a CA integration exists
   (`../features/ca-integration.md` phases 3–5), which is an **ESI** decision about
   integrating with a corporate PKI. Meanwhile the claim is checkable: the trail names who
   recorded it and the CA's own reference.
9. **A recorded remediation needs no second pair of eyes**, because there are no roles yet
   (gap 2). Any operator with the register open can claim a certificate was revoked. *ESI to
   say whether that needs an approver* (`../features/key-lifecycle-and-revocation.md`).
10. **Enforcement of the PIN change is sometimes procedural** — `forcePINChange` needs
   firmware 5.7+, and PIV has no equivalent at all. Under custody model B those keys rely on
   the hand-over term's instruction. The run records which applied, so the exposure is
   measurable; closing it would mean either a 5.7+ fleet or model A (holder present).

Declaring gaps with an adequacy plan is the process the norm anticipates (§5.4.6). A claim of
full conformance that does not survive a look at the code would not be.

---

## References

- NRM v2 (April 2021); G-002 v1.0 (June 2013)
- [`../AGENTS.md`](../AGENTS.md) — the same rules as day-to-day engineering practice
- [`../features/compliance.md`](../features/compliance.md) — the artefacts to produce
- [`../features/secrets-custody.md`](../features/secrets-custody.md),
  [`../features/audit-trail.md`](../features/audit-trail.md),
  [`../features/db-password-and-encryption.md`](../features/db-password-and-encryption.md)
