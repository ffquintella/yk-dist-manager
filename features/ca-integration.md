# Feature: CA integration

## Summary

Get a CSR signed. Three supported issuers: an internal CA for pilots, BastionVault's
PKI engine, and an enterprise CA (AD CS or equivalent) — each with a certificate
profile that produces a usable signing certificate.

## Motivation

The bootstrap's certificate step is only as good as the certificate it gets back. A
self-signed certificate is fine for proving the pipeline works and useless for
signing mail that anybody trusts, because no client trusts its issuer. Which CA
issues determines: whether we build the CSR ourselves or a profile injects the SAN,
what validity and EKUs the certificate carries, and whether revocation is possible
when a key is lost.

## Current state

**Phase 1 done (2026-08-13); phase 2 partly.** The issuer is the **operator**, which
was the decision this feature was waiting for, and it is the only issuer that exists:
the run keeps its PKCS#10 request so it can be saved, the certificate comes back
through the wizard as a file or pasted text, and the run is resumed to import it.
Before the write the certificate is parsed
([`device::certificate`](../src/device/certificate.rs)), summarised on screen, matched
against the slot's public key, and refused unless it carries the holder's
`rfc822Name`. Phases 3–5 — a pilot CA, BastionVault, an enterprise CA — are
automations of this path and are not started; no `trait CertificateIssuer` has been
invented for them, because an abstraction over one implementation is a guess.

## Design

### Option 1 — Internal CA (pilot / lab)

A CA key held by the tool (or beside it), used to sign the CSRs it produces.

- Purpose: prove the whole chain end-to-end — on-device keygen → CSR with SAN →
  issuance → import → the mail client actually offers the certificate — without
  waiting on a PKI ticket.
- Must be **clearly marked as non-production** in the UI and in the audit entry.
  A certificate from a pilot CA that ends up in a production hand-over is worse than
  no certificate.
- The CA key is a secret this tool would then hold, which is exactly what
  `AGENTS.md` says to avoid. Mitigation: pilot mode only, key on the operator's own
  YubiKey (PIV slot 9c of an admin key) rather than in the database, and a hard
  refusal to use it when the template is not marked as a pilot.

### Option 2 — BastionVault PKI (preferred where available)

The unit already runs [BastionVault](https://github.com/ffquintella/BastionVault),
whose PKI secret engine issues certificates from a role. That gives:

- The CA key never touches this tool.
- Roles enforce the profile (allowed SANs, EKUs, validity, algorithms) centrally, so
  the certificate is right by construction rather than by our request.
- Issuance is authenticated and audited on the vault side as well as here.
- A revocation path that already exists.

Integration: sign the CSR through the engine's sign endpoint with a token scoped to
one role, submit our CSR (SAN included), receive the certificate and chain. Because
we build the CSR ourselves, the SAN does not depend on the role injecting it — the
role just has to *allow* it.

### Option 3 — Enterprise CA (AD CS or equivalent)

The institutional PKI, which is what makes a certificate trusted by everyone's mail
client without extra trust distribution.

- Submission is via the CA's own mechanism (certificate template + enrolment agent,
  or an operator-mediated submission and paste-back).
- The SAN may be dictated by the template from the requester's directory identity —
  option B in `features/step-piv-signing-certificate.md`. That is fine, as long as
  the resulting SAN is verified after import rather than assumed.
- Needs a named template, an enrolment path, and (usually) an enrolment-agent
  certificate. All of that is an ESI/PKI-team decision.

### Common requirements, regardless of issuer

The certificate that comes back is **verified before import**:

1. Public key matches the key in slot 9c (`--verify` semantics).
2. `rfc822Name` SAN equals the holder's e-mail, exactly.
3. Subject DN matches what we requested.
4. `digitalSignature` key usage; `emailProtection` EKU when the use case is S/MIME.
5. Chain builds to a trusted root, and the chain is stored with the run.
6. Validity window is sane, and its `not_after` is recorded for renewal tracking.

A certificate failing any of these is refused with a specific reason, not imported
"because the CA said so".

### Offline / manual mode — decided 2026-08-13, and this is what exists

Some institutional CAs cannot be automated. The tool must support: export the CSR to
a file, hand it over, paste or import the issued certificate later, then finish the
run. That means a bootstrap run can be **suspended awaiting issuance** and resumed —
which is a requirement on the executor
(`features/bootstrap-engine.md` Phase 9), not just here.

**The decision of 2026-08-13 makes this the issuer rather than the fallback**: the
tool asks the operator for the certificate they want imported. No endpoint, no
credential, no network. Every other option in this file is an automation of it.

How it runs, end to end:

1. The run produces the PKCS#10 request and **keeps it** in the CSR step's detail.
   Not its size — the request itself, because it has to leave the workstation and a
   request nobody can retrieve means generating a second key and abandoning the
   first. It is a public document: a public key, a name and a signature.
2. The wizard offers *Save the certification request…*, audited as
   `ca.csr.exported` — the moment a request in a holder's name leaves
   the tool.
3. The import step skips, saying what happens next, and the run is recorded as
   **not** completed. A key with no signing certificate must never be claimed as
   ready to hand over.
4. The operator returns with the certificate, loads or pastes it, and sees what it
   says — subject, issuer, serial, validity, every `rfc822Name` — **before** the
   write.
5. *Import the certificate and finish the run* resumes the run. Resuming is what
   makes this possible at all: the pre-flight refuses a fresh run on a configured
   key with no override (`features/device-detection.md` phase 5).

Two consequences worth stating, because both are load-bearing:

* **The PIV PIN has to be typed for the resume.** Nothing is retained (custody
  model B), so the run that set the transport PIN kept no copy of it, and the
  applet will not accept a certificate without it. While the key waits for its
  certificate it is still on the operator's desk with that PIN written down. It is
  supplied to the executor for the run and never recorded — and deliberately not
  handed to the show-once panel, which is for values the *tool* produced.
* **The checks are the feature.** A certificate from an API client arrives
  machine-checked at both ends; one that a person pastes has only the checks here.
  So a mismatch of public key or address is a refusal that names what was expected
  and what arrived, and nothing reaches the key.

## Phases

| # | Phase | State | Notes |
|---|---|---|---|
| 1 | Manual/offline issuer: export the CSR, import the certificate, resume the run | **Done (2026-08-13)** | The decision was to build *this* and no client: the tool asks the operator for the certificate. The CSR is kept in the run so it survives the register being closed; the certificate comes in as a file or pasted text, is checked, and the run is resumed to finish. A `trait CertificateIssuer` was deliberately **not** added — there is one issuer and inventing an abstraction over a single implementation would be a guess at what phases 3–5 need |
| 2 | Post-issuance verification (the six checks above) | **Partly done** | checks 1, 2 and 6 are enforced before the write in [`device::certificate`](../src/device/certificate.rs) and the import step: the public key must be the slot's, the `rfc822Name` must be the holder's, and the validity window is read and recorded. **Checks 3 and 4 — subject DN and key usage / EKU — are now made too**, by the read-back verification (`features/step-piv-signing-certificate.md` phase 7). Only check 5, the chain to a trusted root, is outstanding, and it needs a trust store this tool does not have; it is reported as `chain=unchecked` rather than omitted, so a verification never reads as more complete than it is |
| 3 | Internal pilot CA, clearly marked | Todo | never usable in a production template |
| 4 | BastionVault PKI issuer | Todo | role-scoped token, chain retrieval |
| 5 | Enterprise CA issuer | Todo | needs the template and enrolment path from the PKI team |
| 6 | Chain and trust-anchor storage with the run | Todo | evidence |
| 7 | Renewal: reissue before expiry, on the existing key or a new one | Todo | policy question |
| 8 | Revocation on loss | Todo | `features/key-lifecycle-and-revocation.md` |

## Audit events

| Event | Detail |
|---|---|
| `ca.csr.exported` | `serial=<key> path=<file>` (manual mode) — **implemented** |
| `ca.certificate.issued` | `issuer=<dn> cert_serial=<hex> not_after=<date> ca=<option>` |
| `ca.certificate.rejected` | `reason=san_mismatch|key_mismatch|chain_untrusted…` |

The two `ca.certificate.*` entries are **not** written as separate events today, and
that is deliberate rather than pending: an import is a bootstrap step, so it is
already audited as `bootstrap.step.done` or `bootstrap.step.failed` with the reason,
and the step's detail carries the issuer, the certificate's serial and its validity
window. A second event for the same fact would be a second place for the two to
disagree. They become worth having when a phase-3–5 issuer can succeed or fail
*outside* a run.
| `ca.pilot_used` | A non-production CA signed a certificate |
| `ca.certificate.revoked` | `cert_serial=<hex> reason=<code>` |

## Tests

- Phase 1: behaviour test for the offline round trip — plan → CSR exported → run
  suspended → certificate imported → run completes, with the audit chain intact.
- Phase 2: unit tests over each rejection case, using fixture certificates (SAN
  mismatch, wrong public key, expired, missing EKU).
- Phase 4/5: integration tests against a test CA instance, not a production one.

## Open questions and gates

- ~~**Which issuer for production?**~~ **Answered 2026-08-11: none is hard-wired —
  the CA is a configured parameter**, and **2026-08-13: the operator is the
  issuer.** The manual path is the one that is built; phases 3–5 remain open for a
  deployment that wants its CA automated. The tool must be able to point at any CA, so
  what this feature owes is the *mechanism*, not a choice of issuer: the CSR
  builder is therefore **required**, not optional, and
  [`step-piv-signing-certificate.md`](step-piv-signing-certificate.md) is
  unblocked. Whoever deploys picks the issuer and the endpoint in settings; the
  SAN policy is already configurable
  ([`src/san.rs`](../src/san.rs), [`src/settings.rs`](../src/settings.rs)).
- **Certificate profile** — validity, EKUs, algorithms, revocation endpoints — is the
  PKI owner's decision, with ESI agreement on the integration mechanism (the norm
  requires ESI approval for every integration).
- If the pilot CA is built, its key custody must be agreed before it exists.

## References

- `features/step-piv-signing-certificate.md`, `features/bootstrap-engine.md`
- `docs/security-and-compliance.md`
