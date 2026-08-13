# Feature: Step — PIV signing certificate bound to the holder's e-mail

## Summary

Generate the signing key **on the device** in PIV slot 9c, request a certificate
whose Subject Alternative Name carries `rfc822Name = holder's e-mail`, import the
issued certificate back into the slot, and store the attestation as evidence — so
the key is signing-ready when it is handed over.

## Motivation

"Signing ready to use" is a specific technical claim. For the holder to sign mail in
Outlook or Apple Mail, or a PDF in Acrobat, three things must be true:

1. The private key is on the token and never existed anywhere else.
2. The certificate is in the slot the OS surfaces for signing (9c, *Digital
   Signature*), with `digitalSignature` key usage and, for S/MIME, the
   `emailProtection` EKU.
3. The certificate's `rfc822Name` SAN **matches the mail account**. Clients match on
   the SAN. A certificate with the address only in the DN (`emailAddress=`) is
   treated by most modern clients as not matching the account, so it is offered for
   nothing.

Point 3 is where the tooling gets in the way, and it is the reason this step drives
the native path.

## Current state

**Planned, not executed.** Four plan entries: `PivKeygen`, `PivCsr`,
`PivCertImport`, `Verify`. Native operations: `yubikey::piv::generate`,
`piv::sign_data` over a CSR we build, `certificate::Certificate::write`. The `ykman`
fallback commands are in the plan, with the SAN limitation stated on the step.

## Design

### The SAN problem, stated exactly

`ykman piv certificates request` accepts only `--subject` (an RFC 4514 DN) and
`--hash-algorithm`. There is **no** option for a SAN
(verified against ykman 5.9.2). So the fallback path can produce a CSR with
`CN=Ana Silva,OU=IT,O=Example Organisation` and nothing else. Three ways out:

| Option | How the SAN gets there | Cost |
|---|---|---|
| **A. Build the CSR ourselves** (chosen) | We construct a `CertificationRequestInfo` with the `extensionRequest` attribute carrying `subjectAltName = rfc822Name`, then sign it with `piv::sign_data` using the on-device key | We own the ASN.1; needs `x509-cert`/`der` |
| B. Let the CA inject it | The CA profile/template adds the SAN from the requester's directory identity | Requires a CA that does this, and couples us to its configuration |
| C. Build the certificate directly from the exported public key | Skip the CSR; our internal CA signs a certificate we assemble, then import | Fine for an internal CA, not for an enterprise CA that wants a CSR |

The default is **A**, with B supported for enterprise CAs — see
`features/ca-integration.md`.

### Slot choice: 9c

| Slot | Purpose | PIN behaviour |
|---|---|---|
| 9a | Authentication (Windows logon, SSH) | PIN once per session |
| **9c** | **Digital Signature** | **PIN required for every operation** (per NIST SP 800-73) |
| 9d | Key Management (encryption/decryption) | PIN once |
| 9e | Card Authentication | no PIN |

9c is the right slot for signing precisely because it always asks for the PIN.
Note the interaction: the *slot* enforces per-use PIN regardless of the
`--pin-policy` requested, so the template's `pin_policy = once` is a hint that 9c
overrides — the wizard should say so rather than imply otherwise.

Touch policy `cached` gives a 15-second window after one touch: a sensible balance
for signing several documents without touching for each.

### Algorithm

`eccp256` by default: smaller, faster on the device, and widely supported by mail
clients and PDF signers. `rsa2048` remains available for anything that needs it
(some legacy document workflows). The template parameter drives it, and the certificate
profile must match the CA's supported algorithms.

### Attestation — the evidence that matters

`yubikey::piv::attest` (fallback: `ykman piv keys attest 9c`) returns a certificate,
signed by a Yubico-rooted attestation key on the device, stating that the key in that
slot was **generated on the device** and describing its PIN and touch policies. Storing
it with the bootstrap run is what turns "we generated it on the key" from a claim into
a verifiable fact. This is a required part of the step, not an extra.

### Template parameters

| Parameter | Meaning | Default |
|---|---|---|
| `slot` | PIV slot | `9c` |
| `algorithm` | `eccp256` \| `eccp384` \| `rsa2048` \| … | `eccp256` |
| `pin_policy` | `never` \| `once` \| `always` | `once` (9c enforces per-use) |
| `touch_policy` | `never` \| `always` \| `cached` | `cached` |
| `subject` | RFC 4514 DN | `CN={{holder.name}},OU={{org.unit}},O={{org}}` |
| `san_email` | `rfc822Name` value | `{{holder.email}}` |
| `hash` | CSR signature hash | `sha256` |
| `verify` (import) | Check the certificate matches the slot's key | `true` |

The DN deliberately excludes the e-mail (`features/holder-registry.md`), and a unit
test enforces that.

## Phases

| # | Phase | State | Notes |
|---|---|---|---|
| 1 | Plan entries with subject rendering and the SAN stated on the step | Done | |
| 2 | Native on-device key generation with PIN/touch policy | **Built** (not hardware-verified) | `piv::generate` with `PinPolicy::Always` — consent per signature is the point of this slot |
| 3 | CSR construction with the `rfc822Name` SAN, signed on-device | **Built** (not hardware-verified) | [`device::csr`](../src/device/csr.rs) — option A, `x509-cert` + `piv::sign_data`. Pure assembly with the signature injected, so the ASN.1 is testable with no key; `openssl` reads the SAN back in `tests/interop_csr_san.rs` and verifies the signature. ECDSA only: RSA needs PKCS#1 v1.5 padding applied by the caller and is refused rather than guessed |
| 4 | Submit to a CA and retrieve the certificate | Todo | `features/ca-integration.md` |
| 5 | Import with `--verify` semantics (certificate matches the slot key) | Todo | refuse a mismatch |
| 6 | Attestation capture and storage on the run | **Built** (not hardware-verified) | read at generation time, not later — the proof has to bind to *this* generation. Stored in the step detail; a firmware that cannot attest is recorded as unproven rather than omitted |
| 7 | Verification: read the slot back, check subject, SAN, EKU, chain | Todo | the `Verify` step's real content |
| 8 | Expiry tracking and renewal reminder | Todo | `features/reports-and-export.md` |
| 9 | CHUID / CCC population where a platform needs it | Todo | `ykman` does this on import by default (`--update-chuid`) |

## Audit events

| Event | Detail |
|---|---|
| `bootstrap.step.done` | `step=piv-keygen slot=9c algorithm=eccp256 pin_policy=once touch_policy=cached` |
| `bootstrap.step.done` | `step=piv-csr subject=<dn> san=<email> hash=sha256` |
| `bootstrap.step.done` | `step=piv-cert-import slot=9c serial=<cert serial> issuer=<dn> not_after=<date>` |
| `piv.attestation.stored` | `slot=9c fingerprint=<sha256>` |
| `bootstrap.step.failed` | `step=<id> reason=<status>` |

The certificate's own fields are not secret and belong in the record: they are how a
future audit checks what was issued.

## Tests

- `the_certificate_subject_is_bound_to_the_holder` — DN carries `CN=Ana Silva` and
  `OU=ESI`, and the SAN e-mail is stated on the step.
- `certificate_subject_is_rfc4514_and_excludes_the_email` and
  `rfc4514_special_characters_are_escaped` — a holder named `Silva, Ana` must not
  produce a malformed DN.
- Phase 3: unit tests over the generated CSR bytes — parse it back and assert the
  `rfc822Name` is present and correct, including for a non-ASCII name.
- Phase 5+: behaviour tests against a mock PIV transport, including a certificate that
  does **not** match the slot key (must be refused).
- No test writes to real hardware; manual verification against a test key, with the
  resulting certificate parsed and recorded in the phase notes.

## Open questions and gates

- **Which CA** (blocks Phase 4) — see `features/ca-integration.md`.
- **Certificate profile**: validity period, EKUs (`emailProtection`,
  `clientAuth`?), key usage, CRL/OCSP endpoints. This is a PKI decision, owned by
  whoever runs the CA, and needs ESI agreement.
- Is `emailProtection` enough, or is document signing (Adobe's trust list) also
  required? The answer changes the profile, not the code.
- ~~Whether the OpenPGP reading was the one wanted~~ — **resolved 2026-08-10: this PIV 9c
  path is the chosen mechanism.** `features/step-openpgp-signing-subkey.md` stays specified
  but unscheduled.

## References

- `src/template/plan.rs`, `src/domain/holder.rs`
- `docs/bootstrap-procedure.md`, `docs/yubikey-reference.md`
- [RFC 5280 §4.2.1.6 (SAN)](https://www.rfc-editor.org/rfc/rfc5280#section-4.2.1.6),
  [RFC 8551 (S/MIME)](https://www.rfc-editor.org/rfc/rfc8551)

## The SAN is configurable, because the CA decides where it comes from (2026-08-11)

Roadmap open question 1 — *which CA issues this certificate?* — is still open, and
it does not only decide who signs. It decides **where the SAN comes from**, and
that changes what the tool has to do:

| CA arrangement | Where the `rfc822Name` comes from |
|---|---|
| Internal CA taking our CSR | The request we build — so the CSR builder is required |
| Enterprise CA with a certificate profile | The CA injects it from a directory lookup, and ignores what the request asked for |
| A CA taking the SAN as a separate attribute | Neither: it travels beside the CSR, in a form field or a ticket |

Hard-coding any one of those makes the other two unreachable, and the question
cannot be answered by the implementer. So the SAN is now **a setting**
([`src/san.rs`](../src/san.rs), `AppSettings::san`):

* `pattern` — rendered with the same variables as a bootstrap template,
  defaulting to `{{holder.email}}`, which is what this spec requires. A
  deployment whose CA wants an alias domain or a different form changes it here
  rather than editing every template.
* `source` — `request` (the tool puts it in the CSR), `ca-profile` (the CA
  injects it; the rendered value is what the issued certificate is *checked
  against*), or `out-of-band` (the operator pastes it into the CA's form).

It lives in settings rather than in a template because it follows from the
deployment's CA, not from the procedure being run: every template on one
deployment wants the same answer. A template may still override `san_email` for
the unit that genuinely needs two procedures with different SANs.

Validation happens at **render** time, not at configuration time, because a
pattern is only wrong for some holders: `{{holder.email}}` is fine until it meets
a record without one. A pattern that renders to a name, a DN fragment or a
leftover `{{placeholder}}` is refused — those are the failures that actually
happen.

### Checking what came back

Every one of the three routes can silently produce a certificate with the wrong
SAN or none at all, and the result installs cleanly and looks right everywhere
except when it is used to sign. `san::extraction_guide` is the operator-facing
instructions, shown in the tool rather than only here, and it covers:

```
openssl x509 -in holder.crt -noout -ext subjectAltName

X509v3 Subject Alternative Name:
    email:ana.silva@example.org
```

...reading it straight off the key without a file
(`ykman piv certificates export 9c - | openssl x509 -noout -ext subjectAltName`),
checking the CSR before it is sent, and the three failures worth naming: no SAN
at all, an `email:` that disagrees with the register, and a `DNS:`/`otherName:`
where an `email:` was needed — a UPN
(`otherName 1.3.6.1.4.1.311.20.2.3`) is common on enterprise CAs and is **not** a
substitute for an `rfc822Name`.
