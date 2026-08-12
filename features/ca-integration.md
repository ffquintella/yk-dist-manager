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

**Not started.** The plan's `PivCsr` step names the SAN requirement and notes that on
the `ykman` fallback path the CA must inject it. No issuer is wired up.

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

### Offline / manual mode

Some institutional CAs cannot be automated. The tool must support: export the CSR to
a file, hand it over, paste or import the issued certificate later, then finish the
run. That means a bootstrap run can be **suspended awaiting issuance** and resumed —
which is a requirement on the executor
(`features/bootstrap-engine.md` Phase 9), not just here.

## Phases

| # | Phase | State | Notes |
|---|---|---|---|
| 1 | Issuer abstraction (`trait CertificateIssuer`) + manual/offline mode | Todo | export CSR, import certificate, resume the run |
| 2 | Post-issuance verification (the six checks above) | Todo | refuse on mismatch |
| 3 | Internal pilot CA, clearly marked | Todo | never usable in a production template |
| 4 | BastionVault PKI issuer | Todo | role-scoped token, chain retrieval |
| 5 | Enterprise CA issuer | Todo | needs the template and enrolment path from the PKI team |
| 6 | Chain and trust-anchor storage with the run | Todo | evidence |
| 7 | Renewal: reissue before expiry, on the existing key or a new one | Todo | policy question |
| 8 | Revocation on loss | Todo | `features/key-lifecycle-and-revocation.md` |

## Audit events

| Event | Detail |
|---|---|
| `ca.csr.exported` | `serial=<key> path=<file>` (manual mode) |
| `ca.certificate.issued` | `issuer=<dn> cert_serial=<hex> not_after=<date> ca=<option>` |
| `ca.certificate.rejected` | `reason=san_mismatch|key_mismatch|chain_untrusted…` |
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
  the CA is a configured parameter.** The tool must be able to point at any CA, so
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
