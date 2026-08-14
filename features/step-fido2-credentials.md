# Feature: Step — initial FIDO2 credential, resident on the key

## Summary

Register the key's first FIDO2 credential as a **discoverable (resident)**
credential, so the credential material and its user handle live on the key itself
rather than only in a server-side record.

## Motivation

The requirement is "set up the initial FIDO keys (only on the key)". In WebAuthn
terms that is a discoverable credential created with `residentKey: required`
(`rk=true` at the CTAP layer): the key stores the credential id, the user handle and
the RP id, so it can be used without the relying party first telling the key which
credential to use. That is what makes usernameless and passwordless flows work, and
what makes the key self-contained.

**`ykman` cannot do this.** `ykman fido credentials` only *lists* and *deletes*
credentials — creation is a relying-party operation in the WebAuthn model, and the
CLI does not implement `authenticatorMakeCredential`. So either the credential is
created by an external service during enrolment, or the tool does it itself over
CTAP2. This is the clearest single justification for the native transport.

## Current state

**Planned as native-only.** The plan entry for `StepKind::Fido2Credential` has
`program: None` (no `ykman` fallback exists) and points at
`ctap-hid-fido2::make_credential(rk = true)`. A behaviour test asserts the step is
native-only, so a future refactor cannot quietly give it a fallback that does not
work.

## Design

### What a self-registered credential can and cannot be

A credential created by this tool is created against an **RP id we choose**. That is
useful and limited, and the limits must be stated plainly:

- A credential for RP `example.org` created here **is** a real discoverable credential on
  the key, and is what a subsequent enrolment against a service on that origin can
  use — but only if that service accepts a pre-registered credential (i.e. it is our
  own relying party, or it supports enterprise enrolment).
- A credential cannot be pre-created for a third-party service (Google, Microsoft
  Entra, GitHub). Those services register their own credential against their own RP
  id, at their own enrolment flow. No tool can shortcut that.

So the honest scope of this step is one of:

1. **Our own relying party** — the credential is registered against an internal
   service (for example BastionVault's FIDO2 backend), and the tool records the
   credential id so the service can bind it to the holder.
2. **A placeholder/inventory credential** — proves the applet works and the PIN is
   set, occupies one credential slot, and is deleted before hand-over.
3. **Enterprise attestation** — where the service requires an attested,
   enterprise-issued key, `ykman fido config enable-ep-attestation` (or the CTAP
   equivalent) plus the attestation certificate lets the RP verify the key came from
   us.

Option 1 is the useful one and needs the RP side to exist. The template parameters
are written so that choice is explicit rather than implied.

### Template parameters

| Parameter | Meaning | Default |
|---|---|---|
| `rp_id` | Relying-party id the credential is bound to | `{{org}}` |
| `user_name` | User name stored with the credential | `{{holder.email}}` |
| `resident` | Discoverable credential (`rk=true`) | `true` |
| `uv` | Require user verification at creation | (Phase 3) |
| `alg` | COSE algorithm preference (ES256 / EdDSA) | (Phase 3) |

### Capacity

A YubiKey 5 holds 25 discoverable credentials (100 on firmware 5.7+). The step must
read `get_info` (`remainingDiscoverableCredentials`) and refuse rather than fail
opaquely when the key is full.

### What gets recorded

The credential **id** and the RP id (both non-secret), the algorithm, and whether it
is discoverable. Never the private key — it cannot leave the key — and never the
PIN used to authorise creation.

## Phases

| # | Phase | State | Notes |
|---|---|---|---|
| 1 | Plan entry, native-only, no fallback | Done | asserted by test |
| 2 | `get_info` read: PIN state, remaining credential slots, supported algorithms | **Done** | `remainingDiscoverableCredentials` (CTAP 2.1) is read into `Fido2State::remaining_credential_slots`, and a key with **no free slot** is refused before the PIN is set rather than by the authenticator's own error — which would leave a key that has had a PIN written and nothing else. `None` is "the firmware does not report it", which is not "full": treating it as full would refuse the step on every key below 5.7 |
| 3 | `make_credential` with `rk=true`, UV, algorithm choice | **Done** | **hardware-verified** — the step `ykman` cannot perform at all, and therefore the whole case for the native transport |
| 4 | Record credential id + RP id on the run | **Done** | written into the step's detail as `credential_id=… rp_id=… algorithm=… user_name=…` and read back by [`bootstrap::credential_evidence`](../src/bootstrap/mod.rs), the same idiom the CSR and the attestation use. All four are public by construction: a credential id is what the relying party keeps in its own database, and the private key never leaves the device |
| 5 | List / delete credentials from the GUI | Todo | `ykman` can do this; native is nicer |
| 6 | Enterprise attestation option | Todo | needs an RP that verifies it |
| 7 | Bind the credential to an internal relying party | Todo | depends on which service; BastionVault is the obvious candidate |

## Audit events

| Event | Detail |
|---|---|
| `bootstrap.step.done` | `step=fido2-credential rp=<rp_id> credential=<id> discoverable=true` |
| `bootstrap.step.failed` | `step=fido2-credential reason=<ctap status>` |
| `fido.credential.deleted` | Phase 5, with the credential id |

## Tests

- `scenario_credential_creation_cannot_fall_back_to_ykman` — native-only.
- `credential_registration_is_native_because_ykman_cannot_do_it` — the plan names
  `ctap-hid-fido2`.
- Phase 2+: mock CTAP transport — key full, PIN not set, UV required but unavailable,
  successful creation returning a credential id.
- No test creates a credential on real hardware; manual verification against a test
  key, recorded in the phase notes.

## Open questions and gates

- **Which relying party?** Without an answer, this step can only produce a
  placeholder credential. Decide before Phase 7.
- Is enterprise attestation required by any service we use? It changes the
  procurement requirement (attested keys) as well as the code.
- Deleting a placeholder credential before hand-over must be a deliberate step, not
  a leftover.

## References

- `src/template/plan.rs`
- `docs/yubikey-reference.md`
- [CTAP 2.1 `authenticatorMakeCredential`](https://fidoalliance.org/specs/fido-v2.1-ps-20210615/fido-client-to-authenticator-protocol-v2.1-ps-20210615.html#authenticatorMakeCredential)

### Hardware verification (2026-08-11)

`make_credential` with `rk=true` and user verification, against a YubiKey 5C NFC
(firmware 5.7.4, serial 36668917), produced credential `9acee661...f665` for
`example.org` with ES256. Two touches were required — one for the PIN token, one
for the credential itself.

This is the operation `ykman` cannot perform in any version: it lists and deletes
resident credentials but cannot create one. Until this ran, the native transport
was an argument; it is now a demonstrated capability.

Ordering, learned here: the credential must be created **before** the key is
marked with `forcePINChange`, because the mark takes the PIN out of use. See
`features/step-fido2-pin.md`.
