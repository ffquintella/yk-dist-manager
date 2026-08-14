# YubiKey reference

What the hardware can do, how this tool reaches it, and the traps. Command surface
verified against **ykman 5.9.2** and a **YubiKey 5 NFC, firmware 5.4.3**, on 2026-08-10.

## Capability matrix: native vs `ykman`

| Operation | Native (crate) | `ykman` | This tool uses |
|---|---|---|---|
| List serials | `yubikey` (PC/SC) | `list --serials` | native, fallback ykman |
| Serial + firmware | `yubikey` | `info` | native |
| Model, form factor, per-application enable flags, FIPS state | our own APDU (`device::mgmt`, CCID `00 1D` on AID `A0 00 00 05 27 47 11 17`; no crate covers it) | `info` | native, **built but not hardware-verified** — the parser is pure and covered byte for byte |
| FIDO2 info (PIN set?, retries, credential slots) | `ctap-hid-fido2` | `fido info` | **native, hardware-verified** |
| Set / change FIDO2 PIN | `ctap-hid-fido2` | `fido access change-pin` | **native, hardware-verified** |
| Minimum PIN length (5.7+) | CTAP 2.1 `authenticatorConfig` — crate coverage unconfirmed | `fido access set-min-length` | ykman for now |
| Force PIN change | CTAP 2.1 | `fido access force-change` | ykman for now |
| **Create a FIDO2 credential** | `ctap-hid-fido2` `make_credential(rk=true)` | **✗ impossible** | **native only** |
| List / delete FIDO2 credentials | `ctap-hid-fido2` | `fido credentials list/delete` | either |
| OTP slot status | `hidapi` (protocol ours to write — unwritten) | `otp info` | **ykman**, parsed in `device::ykman::parse_otp_info` |
| OTP access code | `hidapi` (protocol ours to write — deliberately unwritten, see below) | `otp settings <slot> --force --new-access-code -` | **ykman only**, code on **stdin**, built but not hardware-verified |
| PIV PIN / PUK | `yubikey` | `piv access change-pin/change-puk` | native, **built but not hardware-verified** |
| PIV management key | our own APDU (`device::piv_session`; the crate's 3DES type fails on 5.7) | `piv access change-management-key` | native, **hardware-verified 2026-08-11** (the same APDUs, moved into the shared session on 2026-08-13 and not re-run since) |
| PIV on-device keygen | our own APDU (`device::piv_session`; it needs the AES management-key authentication the crate cannot do) | `piv keys generate` | native, **built but not hardware-verified** |
| **CSR with an e-mail SAN** | `device::csr` (`x509-cert`) + `piv::sign_data` | **✗ no SAN option** | **native only** — built; the ASN.1 is verified against `openssl`, the card path is not |
| Import a certificate | our own APDU (`device::piv_session`, `PUT DATA` with command chaining — same authentication problem) | `piv certificates import` | native — built; the certificate comes from the operator (decided 2026-08-13) |
| Attestation | `yubikey::piv::attest` | `piv keys attest` | native, **built but not hardware-verified** |
| Read a slot's certificate back | `yubikey::certificate::Certificate::read` | `piv certificates export` | native, **built but not hardware-verified** — what the `Verify` step checks subject, SAN and key usage against |
| OpenPGP applet | `openpgp-card-sequoia` (to evaluate) | `openpgp *` | undecided |

The two bold "impossible" rows are the reason the native transport is the primary path,
not an optimisation. It has been the *default* build since 0.12.0.

**"Built but not hardware-verified" is a real state, and it is tracked as one.** The PIV
write path was written in a session with no key attached, so every APDU in it is
unexercised. AGENTS.md requires each operation to be exercised against a dedicated test key
before it is relied on, and the feature specs say **Built** rather than **Done** until that
happens.

One thing **nothing** on this table can read: whether an OTP slot carries an **access
code**. Neither the status frame nor `ykman otp info` reports it — the only way to find out
is to attempt a write and be rejected — so no read in this tool claims one, and it is the
register rather than the key that records whether one was set.

## The two things `ykman` cannot do

### 1. Create a FIDO2 credential

`ykman fido credentials` only lists and deletes. Creating a credential is
`authenticatorMakeCredential`, a relying-party operation; the CLI does not implement it. To
put a discoverable credential **on the key** you need a CTAP2 client — hence
`ctap-hid-fido2`. See [`../features/step-fido2-credentials.md`](../features/step-fido2-credentials.md).

### 2. Put an e-mail in a certificate SAN

```
$ ykman piv certificates request --help
Options:
  -P, --pin TEXT
  -s, --subject TEXT              subject … as an RFC 4514 string  [required]
  -a, --hash-algorithm [sha256|sha384|sha512]
```

There is no SAN option. A CSR from `ykman` can only carry a DN. Since mail clients match
on `rfc822Name`, a certificate issued from that CSR is not usable for the holder's mail
unless the **CA** injects the SAN from its own profile. Building the CSR ourselves (or
having the CA inject it) is the only way. See
[`../features/step-piv-signing-certificate.md`](../features/step-piv-signing-certificate.md).

## Verified command surface (ykman 5.9.2)

Kept here because the flags are easy to get subtly wrong.

```bash
# Identification
ykman list --serials
ykman --device <serial> info

# FIDO2
ykman fido info
ykman fido access change-pin [-P <current>] [-n <new>]     # -u for U2F PIN on FIPS 4-series
ykman fido access set-min-length <n>                        # firmware 5.7+
ykman fido access force-change
ykman fido config enable-ep-attestation                     # enterprise attestation
ykman fido config toggle-always-uv
ykman fido credentials list | delete

# OTP  (--access-code must come BEFORE the sub-command)
ykman otp info
ykman otp --access-code <12 hex> settings <1|2> [--delete-access-code]
ykman otp settings <1|2> -A <12 hex>                        # --new-access-code
ykman otp chalresp --generate <1|2> [--touch]
ykman otp static --generate <slot> --length 38
ykman otp yubiotp <slot> --serial-public-id

# PIV
ykman piv info
ykman piv access change-pin  [-P <current>] [-n <new>]
ykman piv access change-puk  [-p <current>] [-n <new>]
ykman piv access change-management-key [-P <pin>] [-a aes256] [--protect] [--generate] [-f]
ykman piv keys generate -a eccp256 --pin-policy once --touch-policy cached <slot> <pubkey.pem>
ykman piv keys attest <slot> <attestation.pem>
ykman piv certificates request -s "CN=…,OU=…,O=…" -a sha256 <slot> <pubkey.pem> <csr.pem>
ykman piv certificates import [-v] [--update-chuid] <slot> <cert.pem>
```

Two flag traps worth memorising: `otp --access-code` is a **group** option that must
precede the sub-command, and `piv access change-puk` uses `-p` for the current PUK while
`change-pin` uses `-P` for the current PIN.

## Firmware gates

| Capability | Minimum firmware |
|---|---|
| AES management key for PIV | 5.4 |
| Per-slot touch policy on OpenPGP | 5.2 |
| `setMinPINLength`, `forcePINChange`, `alwaysUv` | 5.7 (CTAP 2.1 config) |
| 100 discoverable credentials (was 25) | 5.7 |
| Ed25519 / X25519 in PIV | 5.7 |

`domain::YubiKeyRecord::supports_fido_min_pin_length()` and
`device::ykman::supports_min_pin_length()` implement the 5.7 gate; the reference key here
is 5.4.3, so those steps are skipped on it — which is exactly the case the tests cover.

## Factory defaults — none may survive a bootstrap

| Secret | Default |
|---|---|
| PIV PIN | `123456` |
| PIV PUK | `12345678` |
| PIV management key (TDES) | `010203040506070801020304050607080102030405060708` |
| OpenPGP User PIN | `123456` |
| OpenPGP Admin PIN | `12345678` |
| FIDO2 PIN | none set (which is its own problem) |
| OTP slots | unprotected, reprogrammable by anyone |

`ykman piv info` prints `WARNING: Using default Management key!` when the default is still
in place. A unit test asserts none of these values ever appears in a rendered plan, so
nobody can "helpfully" pre-fill them into a template.

## Retry counters and how a key dies

| Applet | Retries | Exhausted ⇒ |
|---|---|---|
| FIDO2 PIN | 8 | Applet blocked; **only** recoverable by resetting FIDO2, which destroys all credentials. There is no PUK. |
| PIV PIN | 3 (configurable) | Blocked; unblock with the PUK |
| PIV PUK | 3 (configurable) | Blocked; only a PIV applet reset, which destroys keys and certificates |
| OTP access code | n/a | Wrong code simply refuses; a lost code freezes the slot |

Consequences for the tool: never "try" a PIN to discover state — read the state first
(`fido info`, `piv info`) and show the remaining retries before and after a step. Note also
that `ykman piv access set-retries` **resets the PIN and PUK to defaults**, so if it is ever
used it must run *before* the PIN change, never after.

## PIV slots

| Slot | Purpose | PIN behaviour |
|---|---|---|
| 9a | Authentication (logon, SSH) | PIN once per session |
| **9c** | **Digital Signature** | **PIN required for every operation** (NIST SP 800-73) |
| 9d | Key Management (decryption) | PIN once |
| 9e | Card Authentication | no PIN |
| 82–95 | Retired key management | |
| f9 | Attestation (Yubico) | read-only |

**`f9` is occupied on every key**, from the factory, and a PIV reset does not clear it. So
`piv::Key::list` (and `ykman piv info`) report a certificate on a key nobody has touched,
and any "has this key been configured?" test must exclude it — `PivState::configured_slots`
is where this tool does that. Getting it wrong is not a subtle bug: it refuses every key.

Slot 9c is chosen for signing because it always asks for the PIN. Note that the slot
enforces that regardless of the `--pin-policy` requested, so a template asking for
`once` on 9c gets per-use behaviour anyway — the wizard should say so rather than imply
otherwise.

Touch policies: `never`, `always`, `cached` (15-second window after one touch).

## Other things that surprise people

- **A protected OTP slot blocks interface mode switching.** Once a slot has an access code
  you cannot change which USB interfaces are enabled until it is removed.
- **`piv certificates import` updates the CHUID by default** (`--update-chuid`), which some
  platforms need and others do not care about.
- **Attestation is the only proof of on-device generation.** Without capturing it, "the key
  was generated on the device" is a claim, not evidence.
- **NFC-enabled keys expose applets over NFC too.** A PIN policy that is fine over USB is
  also the policy over NFC.
- **`ykman` needs the PC/SC service running** on Windows, and `pcscd` on Linux; a
  "no device" error is often a stopped service rather than a missing key.

## References

- [ykman documentation](https://docs.yubico.com/software/yubikey/tools/ykman/)
- [Yubico technical manual (YubiKey 5)](https://docs.yubico.com/hardware/yubikey/yk-tech-manual/)
- [CTAP 2.1](https://fidoalliance.org/specs/fido-v2.1-ps-20210615/fido-client-to-authenticator-protocol-v2.1-ps-20210615.html)
- [NIST SP 800-73-4](https://csrc.nist.gov/publications/detail/sp/800-73/4/final)

## The management applet, and what its absence cost

`device::mgmt` reads CCID instruction `00 1D` (`READ CONFIG`) on the management
application. It is the only source for **which applications are enabled on a key**,
and until it existed the native transport had to leave that field empty — which
caused the same bug twice, in opposite directions:

1. reporting `["PIV"]` (the applet the transport had just spoken to) was read
   downstream as *FIDO2 and OTP are disabled*, and the pre-flight skipped five of the
   standard procedure's eleven steps on a key that had all three enabled;
2. corrected to an empty list, the pre-flight then had to raise a warning on every
   applet-dependent step saying it could not check.

Neither reading of silence is wrong given what was available; both are unnecessary now
the field is read. Three details are load-bearing:

* **The application names match `ykman info`'s exactly** (`Yubico OTP`, `FIDO U2F`,
  `FIDO2`, `OATH`, `PIV`, `OpenPGP`, `YubiHSM Auth`). `bootstrap::preflight` matches on
  these strings, so two transports spelling one application differently would make a
  step skip on one transport and run on the other.
* **The FIPS masks are a different encoding from the capability masks.** One bit per
  application in the order FIDO2, PIV, OpenPGP, OATH, YubiHSM Auth — *not* the
  capability bit values. Reading one with the other's table reports OTP and U2F
  instead, which is wrong in the direction of overclaiming compliance.
* **The declared length is checked.** A truncated response loses its tail, and the tail
  is where the capability masks are — so a partial read would report "no applications
  enabled", which is a claim that skips every step.

## Why the native OTP frame is still unwritten

Every other gap in the matrix above is "not yet"; this one is a decision. No crate in
this dependency graph exposes the Yubico OTP configuration frame, so writing it means
hand-rolling the protocol — frame, CRC, status confirmation — for an operation whose
failure mode is a slot **write-protected by a code nobody holds**, which is not
recoverable from this tool. `features/step-otp-access-code.md` phase 4 keeps it unwritten
until there is a key to verify it against, and the `ykman` path exists so the step is not
blocked meanwhile.

What that path costs, and the reason the step and the pre-flight both say so: `ykman otp
settings` **rewrites the slot's other settings to their defaults**, and it refuses an
**empty** slot outright — an access code protects a configuration, and an empty slot has
none. Both facts come from `ykman` 5.9.2's own source rather than from experiment.
