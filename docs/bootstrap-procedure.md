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
| OTP access code | Nobody — generated and discarded, slot frozen | n/a |

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
   mid-run.
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

Replaces the default TDES management key with a **random AES-256 key stored on the key
itself, guarded by the PIN**, so there is nothing to hold in custody.

```
native:  yubikey → MgmKey::set_protected
ykman:   ykman piv access change-management-key --algorithm aes256 --protect --generate --force --pin <PIN>
```

Parameters: `algorithm` (`aes256`), `protect` (`true`).

### 8. PIV key generation, slot 9c — *required*

Generates the signing key **on the device**. The private key never exists anywhere else,
and `piv::attest` can prove that afterwards.

```
native:  yubikey → piv::generate(slot 9c, ECCP256, pin_policy, touch_policy)
ykman:   ykman piv keys generate -a eccp256 --pin-policy once --touch-policy cached 9c pubkey.pem
```

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
If issuance is offline, the run suspends and resumes when the certificate comes back.

### 10. Certificate import, slot 9c — *required*

Writes the issued certificate into the slot, verifying it matches the slot's key.

```
native:  yubikey → certificate::Certificate::write
ykman:   ykman piv certificates import --verify 9c cert.pem
```

Before importing, check: public key matches slot 9c, `rfc822Name` equals the holder's
e-mail exactly, subject matches what was requested, `digitalSignature` key usage,
`emailProtection` EKU where S/MIME is the use case, and the chain builds. A certificate
failing any of those is refused with the specific reason.

### 11. Verification — *required*

Reads the key back and stores the end state as evidence: FIDO2 PIN present, credential
count, OTP slot protected, PIV slot 9c occupied with the expected subject and SAN, plus the
attestation certificate.

```
native:  yubikey → piv keys/certificates + ctap-hid-fido2 → get_info
ykman:   ykman piv info; ykman fido info; ykman otp info
```

This is what turns "we ran the procedure" into "here is what the key contains".

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

## What the record ends up saying

For each run: template id and version, operator, key serial, holder, start and end time,
every step's outcome with a secret-free detail line, the custody model and per-step change
enforcement, and the attestation. That is what gets attached to the hand-over, and what answers "what was
applied on the bootstrap" a year later.

## Variants

- **`fido-only`** — FIDO2 PIN, minimum PIN length, forced change, credential, verification.
  For keys that only need WebAuthn.
- **`org-sysadmin`** (planned) — adds an SSH credential
  ([`../features/ssh-authentication.md`](../features/ssh-authentication.md)).
- **Stock preparation** (planned) — everything that does not need a holder, so keys can be
  prepared in a batch and assigned later.
