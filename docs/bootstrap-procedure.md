# The bootstrap procedure

The `org-standard` template, step by step. This document and
[`BootstrapTemplate::org_standard()`](../src/template/mod.rs) describe the same thing and
must be changed together.

`org-standard` is the procedure this build *ships*, not the only one a unit can run: the
**Templates** screen adds, duplicates and edits templates, and an edit is stored as a new
version rather than replacing this one ([`../features/bootstrap-templates.md`](../features/bootstrap-templates.md)).
So read what follows as the default, and read the version recorded on a bootstrap run for
what a particular key actually got.

> **Status:** the wizard builds and records this plan today. Execution against a key lands
> in Wave 1 ([`../roadmap.md`](../roadmap.md)); the "Execute on key" button is disabled and
> says so.

## Custody: what happens to the secrets

**Model B, decided 2026-08-10.** Every secret the operator sets is a **transport**
secret: the holder replaces it on first use, and this tool retains nothing.

| Secret | Who ends up with it | How the change is enforced |
|---|---|---|
| FIDO2 PIN | Holder replaces the transport PIN | `forcePINChange` on firmware **5.7+**; instructed on the term below that |
| PIV PIN | Holder replaces the transport PIN | Instructed on the term — PIV has no force-change flag |
| PIV PUK | Handed over in the same sealed envelope | Instructed |
| PIV management key | Nobody — random, on the key, PIN-guarded | n/a |
| OTP access code | The holder, on the sealed slip | Destroyed with the slip; nothing retained |

The transport secrets must reach the holder out of band: in person, or a sealed printed
envelope. Never the e-mail the key's own certificate protects.

Each run records the model (`transport-pin+forced-change`) and, per step, whether the
change was `enforced-by-firmware` or `instructed-on-handover` — so an audit can tell the
two apart, and a report can list the keys where it was only instructed. Details and the two
sub-decisions still open (the PUK, the OTP access code) are in
[`../features/secrets-custody.md`](../features/secrets-custody.md).

## Before the first step

1. **Read the key.** Serial, model, firmware and enabled applications come from the
   hardware, never from typing. A wrong serial mis-attributes a credential.
2. **Pick the holder.** Their corporate e-mail is what the signing certificate will carry;
   validate it *before* generating anything, because a wrong SAN means a reissue.
3. **Check the gates.** Firmware below 5.7 has no minimum-PIN-length policy; a key with PIV
   disabled cannot take a certificate. These become skips, shown up front, not failures
   mid-run. Which applications are enabled is only known when the key was read through
   `ykman` — the native path cannot see the management applet — and an *unknown* list is
   never read as "disabled": those steps are attempted, with a warning saying so.
4. **Review the plan.** Every step, its transport (`native` / `ykman` / `manual`) and its
   caveats, on screen, before anything is written.

## The eleven steps

### 1. FIDO2 PIN — *required*

Sets the PIN that guards FIDO2. A key with no PIN cannot do user verification at all, so
this is the step that makes it a real second factor.

```
native:  ctap-hid-fido2 → authenticatorClientPIN(setPIN)
ykman:   ykman --device <serial> fido access change-pin --new-pin <FIDO2-PIN>
```

Read `fido info` first to know whether a PIN already exists (`setPIN` vs `changePIN`) —
guessing costs one of the **8** retries, and exhausting them means resetting FIDO2 and
losing every credential. There is no PUK for FIDO2.

Parameters: `min_length` (6), `source` (`operator-entered`).

Under model B this is a **transport** PIN — step 3 is what obliges the holder to replace it.

### 2. FIDO2 minimum PIN length — *optional, firmware 5.7+*

Raises the floor so a later PIN change cannot weaken it.

```
ykman:   ykman --device <serial> fido access set-min-length 6
```

**Irreversible** short of a FIDO2 reset, and skipped automatically below 5.7 (the reference
key here is 5.4.3, so it is skipped).

### 3. Forced PIN change — *optional, firmware 5.7+*

Marks the FIDO2 PIN so the key refuses to be used until the holder changes it. This is the
mechanism custody model B rests on.

```
native:  ctap-hid-fido2 → authenticatorConfig(forcePINChange)
ykman:   ykman --device <serial> fido access force-change
```

Below firmware 5.7 the flag does not exist: the same procedure runs, the hand-over term
carries the instruction instead, and the run records
`enforcement=instructed-on-handover` rather than claiming an enforcement the key cannot
provide. (The reference key here is 5.4.3, so this is the common case today.)

### 4. OTP slot access code — *required*

Writes the 6-byte code that write-protects OTP slot 1. Without it, anyone who plugs the key
in can reprogram the slot to type whatever they like.

```
ykman:   ykman --device <serial> otp settings 1 --new-access-code <12 hex> --force
```

Still on the fallback path: no Rust crate implements the OTP configuration protocol. Two
things to know: the code is exactly 6 bytes (12 hex characters), and **a protected slot
blocks USB interface mode switching** until the code is removed.

Parameters: `slot` (1), `source` (`generated`).

### 5. Initial FIDO2 credential, resident on the key — *required*

Registers a **discoverable** credential (`rk=true`), so the credential id, user handle and
RP id live on the key itself.

```
native:  ctap-hid-fido2 → authenticatorMakeCredential(rk = true)
ykman:   impossible — the CLI can only list and delete credentials
```

Scope matters: a credential is created against **an RP id we choose**. That is useful for
our own relying party; it cannot pre-register the key with a third-party service, which
always runs its own enrolment. See
[`../features/step-fido2-credentials.md`](../features/step-fido2-credentials.md).

Parameters: `rp_id` (`{{org}}`), `user_name` (`{{holder.email}}`), `resident` (`true`).

### 6. PIV PIN and PUK — *required*

Replaces both factory defaults (`123456` / `12345678`). Changing only the PIN is pointless:
the PUK resets the PIN.

```
native:  yubikey → YubiKey::change_pin, change_puk
ykman:   ykman piv access change-pin --pin <old> --new-pin <new>
         ykman piv access change-puk --puk <old> --new-puk <new>
```

3 retries each. Exhausting the PIN needs the PUK; exhausting the PUK needs an applet reset
that destroys the keys and certificates.

Under model B both are transport secrets handed over in the sealed envelope, with the term
instructing the holder to change the PIN. PIV cannot enforce that, so the wording of the
term is the only mechanism.

### 7. PIV management key — *required*

Replaces the default management key with a **random key stored on the key itself,
guarded by the PIN**, so there is nothing to hold in custody.

```
native:  device::piv_mgm → GENERAL AUTHENTICATE (AES) + SET MANAGEMENT KEY + PUT DATA 5FC109
ykman:   ykman piv access change-management-key --algorithm aes256 --protect --generate --force --pin <PIN>
```

**Not `MgmKey::set_protected`, and not AES-256.** Two measured corrections: the slot
takes 24 bytes, so the key is AES-192 on current firmware; and the `yubikey` crate
cannot authenticate to a 5.7 management slot at all, because its `MgmKey` sends a 3DES
algorithm identifier and 5.7 removed 3DES. The algorithm is **read** from the card
rather than chosen, because guessing it fails in a way indistinguishable from a wrong
key. The run records which algorithm was used.

Parameters: `protect` (`true`). `algorithm` is not honoured — the slot's own algorithm
wins.

### 8. PIV key generation, slot 9c — *required*

Generates the signing key **on the device**. The private key never exists anywhere else,
and `piv::attest` can prove that afterwards.

```
native:  device::piv_session → GENERATE ASYMMETRIC KEY PAIR (slot 9c, ECCP256, policies)
ykman:   ykman piv keys generate -a eccp256 --pin-policy once --touch-policy cached 9c pubkey.pem
```

Generation needs management-key authentication, and that authentication belongs to the
**card session** — so it is issued on the same connection that authenticated, rather
than through the crate, for the reason given in step 7.

Slot 9c is *Digital Signature*: it requires the PIN for **every** signature by design, which
is what you want for signing and what the wizard should say plainly (the slot overrides a
`once` policy request).

Parameters: `slot` (9c), `algorithm` (`eccp256`), `pin_policy` (`once`),
`touch_policy` (`cached` — one touch covers 15 seconds).

### 9. Certificate request, with the e-mail SAN — *required*

Produces a CSR for the generated key, carrying:

- Subject: `CN={{holder.name}},OU={{org.unit}},O={{org}}` (RFC 4514, escaped)
- SAN: `rfc822Name={{holder.email}}` ← **the part that makes it usable**

```
native:  build a CertificationRequestInfo with the SAN extensionRequest,
         sign it with yubikey → piv::sign_data
ykman:   ykman piv certificates request -s "CN=…" -a sha256 9c pubkey.pem csr.pem
         (no SAN option — the CA must inject it)
```

The e-mail is deliberately **not** in the DN: modern clients match on the SAN, and a
deprecated `emailAddress` RDN gets the certificate offered for nothing. A unit test asserts
the DN contains no `@`.

Then the CSR goes to a CA ([`../features/ca-integration.md`](../features/ca-integration.md)).
**The operator takes it there** (decided 2026-08-13): the run keeps the request, the
wizard offers *Save the certification request…*, and the run stops with the import
pending — recorded as **not** completed, because a key with no signing certificate is
not ready to hand over.

### 10. Certificate import, slot 9c — *required*

Writes the issued certificate into the slot, verifying it matches the slot's key.

```
native:  device::piv_session → PUT DATA (certificate object for the slot, chained)
ykman:   ykman piv certificates import --verify 9c cert.pem
```

The certificate arrives **by hand** — loaded from a file or pasted into the wizard —
so this is where the checking happens rather than at an API boundary. The operator
sees what the certificate says (subject, issuer, serial, validity, every
`rfc822Name`) before the write, and the run is **resumed** to perform it: the
pre-flight refuses a fresh run on a key that is already configured.

Enforced before the write: the public key matches slot 9c
(`device::certificate::must_match_public_key`), the `rfc822Name` includes the holder's
e-mail, and the validity window is read and recorded. The subject DN and the key usage
/ EKU are checked too, by the verification step below, which reads the certificate back
off the card rather than trusting what was just sent to it. Only the **chain** is
outstanding, and it needs a trust store this build has not got — see
`features/ca-integration.md` phase 2. A certificate failing any check is refused with
the specific reason, and nothing reaches the key.

### 11. Verification — *required*

Reads the key back and stores the end state as evidence: FIDO2 PIN present, credential
count, both applets' retry counters, OTP slot protected, the attestation certificate,
and **the certificate in slot 9c read back off the card and checked**.

```
native:  yubikey → piv keys/certificates + ctap-hid-fido2 → get_info
ykman:   ykman piv info; ykman fido info; ykman otp info
```

This is what turns "we ran the procedure" into "here is what the key contains".

Four checks on the read-back certificate, each reported separately because an operator
needs to know *which* disagreed:

| Check | What fails it |
|---|---|
| `subject` | the slot holds a name this run did not ask for. Compared as a set of attributes, since a CA may reorder and re-space a DN |
| `rfc822Name` | the holder's address is not among the SANs — signatures made with it would not validate against it |
| `key usage` | an **encryption** certificate in a signing slot: the realistic CA mix-up, which imports cleanly and then fails every signature. Reported as *unchecked* when the certificate carries neither extension, because that constrains nothing |
| `chain` | **always unchecked**: it needs a trust store this build has not got. Reported rather than omitted, so a verification never reads as more complete than it is |

A failed check **fails the step**, and the step is required — so a run whose
certificate does not match what it asked for does not reach `Completed`, and the key is
not offered for hand-over. An empty slot is not a failure: it is the shape of a run
whose certificate has not come back from the CA yet, and step 10 has already said so.

## Ordering traps

These are in the executor, not in the operator's head:

1. `piv access set-retries` **resets the PIN and PUK to defaults** — so if it is used at
   all, it runs *before* the PIN change.
2. The PIV PIN must change before key generation, since generation authenticates with it.
3. The management key must be in place before generation (generation is a management
   operation).
4. The certificate can only be imported after issuance; the run may have to wait.
5. The OTP access code goes last among the OTP steps: once a slot is protected, further
   changes need the code.
6. An access code can only be written to a slot that **holds a configuration** — the
   code write-protects a configuration, and `ykman otp settings` refuses an empty slot
   outright. On a key whose OTP slots are both empty the step reports that and skips,
   rather than failing halfway through the procedure.

## What the record ends up saying

For each run: template id and version, operator, key serial, holder, start and end time,
every step's outcome with a secret-free detail line, the custody model and per-step change
enforcement, and the attestation. That is what gets attached to the hand-over, and what answers "what was
applied on the bootstrap" a year later.

## Variants

- **`fido-only`** — FIDO2 PIN, minimum PIN length, credential, forced change, verification.
  For keys that only need WebAuthn. Its steps are a filtered view of `org-standard`,
  order included — so the forced change is last here for the same reason, and its
  version is taken from that procedure rather than numbered on its own.
- **`org-sysadmin`** (planned) — adds an SSH credential
  ([`../features/ssh-authentication.md`](../features/ssh-authentication.md)).
- **Stock preparation** (planned) — everything that does not need a holder, so keys can be
  prepared in a batch and assigned later.
