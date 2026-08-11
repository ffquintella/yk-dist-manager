# Feature: SSH authentication from the key

## Summary

Optional: make the distributed key usable for SSH, using the PIV authentication slot
(9a) through PKCS#11, or a FIDO2 resident SSH key (`ed25519-sk`).

## Motivation

The people who receive these keys are, in this unit, the people who administer servers.
A key that already carries a signing certificate can also carry the credential that gets
them onto a host, which removes a private key file from a laptop — the most commonly
copied secret in any infrastructure team.

It is explicitly *optional*: it is not part of the original requirement, and it adds a
dependency on how SSH access is granted, which is not this tool's business.

## Current state

**Not started.** Slot 9a is untouched by the default template.

## Design

### Two mechanisms

| | PIV slot 9a + PKCS#11 | FIDO2 `ed25519-sk` |
|---|---|---|
| Client needs | `ssh-keygen -D <pkcs11 lib>` or `PKCS11Provider` | OpenSSH 8.2+ |
| Server needs | The public key in `authorized_keys` (or a CA-signed certificate) | The `sk-ssh-ed25519@openssh.com` public key in `authorized_keys` |
| Touch | PIV touch policy | Always (user presence) |
| PIN | PIV PIN | FIDO2 PIN |
| Resident on key | Yes | Yes, if created with `-O resident` |
| Extra tooling | `yubico-piv-tool` / OpenSC PKCS#11 module | none |

`ed25519-sk` is simpler and needs no PKCS#11 module on every client, which for a fleet of
laptops is the deciding factor. PIV 9a is the answer where the server side is already
built around X.509 or where a certificate authority signs host access.

### What this tool would do

For the FIDO2 route: create a resident SSH credential during bootstrap (which is the same
`make_credential` machinery as `features/step-fido2-credentials.md`, with the
`ssh:` RP id convention OpenSSH uses), then export the public key so it can be added
wherever authorised keys are managed.

For the PIV route: generate in 9a with a touch policy, request or self-sign a certificate,
and export the SSH public key form of it.

In both cases the tool's job ends at "here is the public key, and here is the record of
which key it lives on". Granting access is somebody else's system, and the honest
integration point is an export plus a record — not a write into `authorized_keys`.

### Interaction with the rest of the template

Adding this to the default template means a third credential and possibly a third PIN
prompt during hand-over. It belongs in a separate template (`org-sysadmin`), not bolted
onto the standard one.

## Phases

| # | Phase | State | Notes |
|---|---|---|---|
| 1 | Decide the mechanism (`ed25519-sk` vs PIV 9a) | Todo | depends on how SSH access is granted |
| 2 | `StepKind::SshCredential` + template `org-sysadmin` | Todo | separate template, not the default |
| 3 | Resident `ed25519-sk` credential creation | Todo | reuses the FIDO2 credential machinery |
| 4 | Public-key export in `authorized_keys` format | Todo | the deliverable to whoever manages access |
| 5 | PIV 9a alternative with touch policy and certificate | Todo | for X.509-based access |
| 6 | Record the public key and its fingerprint on the run | Todo | so a lost key's SSH access can be revoked |
| 7 | Revocation path: which hosts/systems carry this public key | Todo | `features/key-lifecycle-and-revocation.md` |

## Audit events

| Event | Detail |
|---|---|
| `bootstrap.step.done` | `step=ssh-credential type=ed25519-sk resident=true fingerprint=…` |
| `ssh.pubkey.exported` | `fingerprint=… path=…` |
| `ssh.access.revoked` | Phase 7, per system |

## Tests

- Unit: the exported public key parses as a valid `authorized_keys` line, and the
  fingerprint matches what `ssh-keygen -lf` computes for the same key material (fixture
  based).
- Behaviour: the sysadmin template produces the SSH step and the standard template does
  not.
- No hardware writes in tests.

## Open questions and gates

- Whether SSH access at all is in scope for this tool, or whether it belongs to whatever
  manages server access (BastionVault's SSH brokering is a candidate, and if it is used,
  this feature may be unnecessary).
- Phase 7 needs an inventory of where a public key was authorised, which the tool cannot
  know unless it is told. Without it, "revoke SSH access for this lost key" stays a manual
  hunt.

## References

- `features/step-fido2-credentials.md`, `features/key-lifecycle-and-revocation.md`
- [OpenSSH FIDO/U2F support](https://www.openssh.com/txt/release-8.2)
