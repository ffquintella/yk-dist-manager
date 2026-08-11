# Feature: Hosting the database on an SMB share

## Summary

Let the operator name an **SMB share** — `smb://fileserver/ti-share/yubikeys/keys.sqlite3`
or `\\fileserver\ti-share\yubikeys\keys.sqlite3` — and have the application reach it
itself: connect with the credentials the operator is **already signed in with**
(the default, and on Windows the whole answer), as a **guest** where the share allows
it, or with a **named account** whose password is typed at the chooser and never
stored. The database then opens from the local path the connection produced, in the
network-share locking mode that already exists.

Implemented in [`src/store/smb/`](../src/store/smb/), driven from
[`src/app.rs`](../src/app.rs) and the chooser in
[`src/ui/database.rs`](../src/ui/database.rs).

## Motivation

Every other storage spec in this repository ends with the same sentence: *a real
network share is still the recommendation*. Until now the application could not
help an operator get one. It could only open a path, so the share had to be
mounted first — by Explorer, by Finder, by `fstab`, by a login script — and the
symptom of that not having happened was the message
"`… is not reachable — is the share mounted?`", which names the problem and offers
nothing.

Three things go wrong in practice, and all three are the same missing capability:

| Situation | Before | Now |
|---|---|---|
| A domain workstation, the register on the unit's file server | The operator maps a drive first, or the tool cannot see the file. A mapped letter also drifts: `Z:` is a different share on the next workstation | The UNC path is opened directly with the signed-in user's credentials. No drive letter, nothing to drift |
| A small NAS with a guest-readable share | Nothing — the tool has no way to ask for guest access | Anonymous connection, chosen deliberately and labelled as such in the status line |
| A share that needs a *different* account from the workstation login | The password goes into a mapped drive, remembered by the OS for every application | Typed at the chooser, used for the connection, and dropped. Never written to the settings file, the database, a log or an audit entry |

This is also the answer to the open question in
[`cloud-sync-hosting.md`](cloud-sync-hosting.md): the installation that keeps the
register in OneDrive does so because getting to the unit's share was harder than
not. Making the share reachable from inside the application is what makes
"use a real share instead" an instruction somebody can follow.

## Design

### The share is connected; the database is still just a file

The connection and the database are deliberately separate steps, in this order:

1. **Parse** the location the operator typed into a [`ShareTarget`] — host, share,
   and the path *within* the share.
2. **Attach** the share through a [`Connector`], which returns the local path its
   root is reachable at (`\\fileserver\ti-share` on Windows,
   `/Volumes/ti-share` on macOS) and whether *this process* made the connection.
3. **Open** the database at `<root>/<path within the share>` with the existing
   `Store::open_existing` / `Store::create_new`, which classify that path as
   `Location::NetworkShare` and apply the rollback-journal pragmas.

So `Store` learns nothing about SMB. The share is a thing the application holds
open around the database, which keeps the storage code — the code with the
coverage gate and the audit obligations on it — unchanged, and means a database on
a share mounted by the operating system behaves exactly like one on a share
mounted by this application.

### An already-reachable share is used, never re-connected

Every connector probes first. If the share is already mounted — by Finder, by a
login script, by the operator five minutes ago — that mount is used as it is, and
the attachment is marked as **not ours**. Nothing then tears it down on close: a
tool that unmounted the share an operator had mounted for their own work would be
worse than one that could not mount at all.

### Credentials

```
Access::LoggedOnUser   the credentials the operator is already signed in with
Access::Anonymous      guest: no user, no password
Access::Named          a named account, password typed every time
```

`LoggedOnUser` is the default, and on **Windows** it is the whole mechanism: a UNC
path is opened with the session's own token, so the correct implementation is to
make no API call at all and let Windows authenticate as it already does for
Explorer. On **macOS** it means the Keychain entry for that server, which is what
the operating system offers and the only credential the application is entitled to
reuse without asking.

An explicit choice is honoured **exactly**. The logged-on user is tried first only
when that is what the operator asked for (or has not chosen, which is the same
default), never as a silent fallback under a named account: connecting as the
wrong identity is not a convenience, it is a share opened with permissions nobody
reviewed — and on a read-only-for-everyone share it would look like the register
had lost its writes.

### The password

`Credential::Named` holds its password in a [`Secret`], which:

- keeps it as bytes, overwritten with zeros on drop;
- prints as `Secret(********)` from `Debug`, so no `{:?}` anywhere can leak it —
  including the `tracing` fields that log a failed connection;
- is readable only from inside the crate (`pub(crate) fn expose`), so a test
  cannot assert on it and a UI cannot echo it;
- never reaches an argument vector. This is why the platform backends are native
  API calls rather than `net use` and `mount_smbfs`: a password in `argv` is
  readable by every process on the workstation, and one in a credentials file is
  the temporary file AGENTS.md §2 forbids.

The settings file records the share, the access mode and the **user name**. It
never records a password — the same rule, and the same reason, as the database
password: the file sits next to the register.

### Platform backends

| Platform | Connect | Disconnect | Notes |
|---|---|---|---|
| Windows | `WNetAddConnection2W` (`windows-sys`) | `WNetCancelConnection2W` | Deviceless connection: no drive letter is mapped, and the UNC path works afterwards. `LoggedOnUser` makes no call at all |
| macOS | `NetFSMountURLSync` (NetFS.framework, via `core-foundation`) | `/sbin/umount <mountpoint>` | The mountpoint comes back from the framework; existing mounts are found by parsing `/sbin/mount` |
| Linux, other | — | — | Refused with the instruction: an unprivileged process cannot mount CIFS, so the share must come from `mount.cifs`, `autofs` or the desktop's own mounter. An already-mounted share works normally |

The Linux gap is stated rather than papered over. `mount.cifs` needs root or a
`setuid` helper, and the alternative — a credentials file — is exactly the
"secret in a temporary file" this project does not do. What Linux gets is the
probe: a share already mounted at `/mnt/ti-share` is found and used.

Windows error codes are translated, because the numbers are the difference between
three completely different operator actions:

| Code | What the operator is told |
|---|---|
| `ERROR_LOGON_FAILURE`, `ERROR_INVALID_PASSWORD` | the user name or password was refused |
| `ERROR_ACCESS_DENIED` | authenticated, but this account may not use the share |
| `ERROR_BAD_NETPATH`, `ERROR_BAD_NET_NAME` | the server or the share name is wrong |
| `ERROR_SESSION_CREDENTIAL_CONFLICT` | Windows already has a connection to that server as another user — that one has to go first |
| `ERROR_NO_NETWORK` | there is no network |

### Where the mount point comes from, and why it is not chosen here

Neither backend picks a directory. Windows connections are deviceless, so the
"mount point" is the UNC path itself. macOS is told to mount with no explicit
path, which puts the share under `/Volumes/<share>` through the automounter and
reports back where it landed — the same thing Finder does. Choosing a directory
would mean creating one, deciding what to do when it is already occupied, and
cleaning it up after a crash: three problems the operating system has already
solved.

### Personal data

A user name and a server name, in the settings file and in the audit entry that
records the connection — the same identity the audit trail already carries. No
password, on any path, in any file.

## Current state

**Shipped**, phases 1–8. A location parses, a credential is built, `ShareConnection`
attaches and releases, all three platform backends exist, the chooser has an
*Open from a network share (SMB)* card, Settings shows which share is held and as
whom, and `--diagnose` reports how this build reaches a share. What is left is
reconnecting a share that drops mid-session (phase 9) and Kerberos on macOS
(phase 10).

## Phases

| # | Phase | State | Notes |
|---|---|---|---|
| 1 | `ShareTarget` parsing: `smb://`, UNC, `//host/share`, with the traversal and length rules | Done | rejects `..`, empty host or share, control characters |
| 2 | `Access` / `Credential` / `Secret`, and the settings entry that remembers a share without its password | Done | `Debug` redaction and zero-on-drop are asserted by tests |
| 3 | `Connector` trait, `ShareConnection` (RAII), probe-before-connect, `MockConnector` | Done | the whole flow is testable with no server |
| 4 | Windows: `WNetAddConnection2W`, deviceless, with the error-code translation | Done | `LoggedOnUser` makes no call; the file is verified to compile for `x86_64-pc-windows-msvc` |
| 5 | macOS: `NetFSMountURLSync`, existing-mount detection by parsing `/sbin/mount` | Done | the parser is platform-independent and tested everywhere |
| 6 | Linux / other: probe only, with the instruction that names the alternative | Done | refusal, not a silent failure |
| 7 | GUI: the share card in the chooser, remembered shares, share state and *Close and disconnect* in Settings | Done | the identity is a radio with a sentence each, and the password field appears only for a named account |
| 8 | `--diagnose` reports the connector this build has and the shares this workstation used | Done | |
| 9 | Reconnect a dropped share mid-session | Todo | today a share that goes away surfaces as an SQLite error and a close; the register is not lost, but the operator has to reconnect by hand |
| 10 | Kerberos / explicit domain-controller selection on macOS | Todo | NetFS can be told to use Kerberos; nobody has asked, and it needs a domain to test against |

## Audit events

| Event | When |
|---|---|
| `db.share.connected` | Written immediately after the database on a freshly connected share opens. Names the share and the identity used (`the signed-in user`, `guest`, or the user name) — never a password |
| `db.share.disconnected` | Written **before** the database closes, while there is still a database to write to, whenever this session is about to take down a connection it made |

A connection that **fails** has no audit entry and cannot have one: there is no
open database to write it to. It is logged (`db.share.connect.failed`,
`db.share.connected`, `db.share.detach.failed`) and shown on the chooser — the
same rule as a refused open in
[`database-selection.md`](database-selection.md).

## Tests

`tests/unit_store_smb.rs`:

- every accepted spelling of a location parses to the same target — `smb://`,
  `cifs://`, `\\host\share\…`, `//host/share/…`, mixed separators, a trailing
  slash;
- `smb://DOMAIN%5Cuser@host/share` and `smb://user@host/share` carry the user out,
  and the user is *not* part of the target's identity;
- a location with no share, an empty host, a `..` segment, a NUL or a control
  character is refused, and the message says which;
- a location longer than `MAX_NOTE` is refused;
- `Secret` prints redacted from `Debug` and is zeroed when dropped;
- `Credential::describe()` never contains the password;
- the default access is `LoggedOnUser`, and on Windows it makes no connection call
  (asserted through the mock connector's call log);
- an explicit named credential is used as given and is **not** silently replaced
  by the logged-on user;
- an already-reachable share is adopted rather than connected, and is not detached
  on close;
- a share this process connected *is* detached on close, once;
- a failed connection is reported with the target in the message and nothing is
  left attached;
- the macOS `/sbin/mount` parser finds an smbfs mount of the right host and share,
  ignores other filesystems, and is case-insensitive about the host;
- the unsupported-platform connector refuses with the alternative named.

`tests/behaviour_smb_share.rs` (6):

- `scenario_a_register_on_a_guest_share_is_created_written_and_reopened` — connect
  anonymously through the mock, create the register on the share, record a key and a
  holder, close, reopen, and find them;
- `scenario_a_share_hosted_register_avoids_wal_and_leaves_no_sidecars` — the
  journal mode follows the *fact* that the file is on a share, not the spelling of
  the mount point;
- `scenario_a_typo_on_a_share_is_refused_rather_than_creating_a_second_register`;
- `scenario_a_named_credential_is_remembered_by_identity_and_never_by_password` —
  the settings file on disk carries the share, the mode and the user, and does not
  carry the password;
- `scenario_a_share_the_operator_had_already_mounted_survives_the_register_closing`;
- `scenario_a_refused_share_opens_no_database_and_names_what_to_fix`.

`src/settings.rs` unit modules cover the remembered share: the round trip, the cap,
normalisation of a hand-edited list, and the assertion that no password field is
ever serialised.

`tests/behaviour_app_smb_share.rs` (1 scenario, the only test that drives
`YkDistApp` for this feature — it owns its binary's environment, and
`app.share_connector` is replaced with a mock so no file server is involved):
connecting as a named account creates the register on the share, opens it as
`Location::NetworkShare`, clears the password from the form, and writes
`db.share.connected` naming the share and the account and **not** the password; the
share is remembered without its password; closing writes `db.share.disconnected`
*before* `db.closed`; a share the operator had already mounted survives the close; a
share with no file named in it, a location whose account contradicts the chosen
identity, and a refused credential are each refused with the fix in the message and
nothing opened; and quitting releases the share.

`src/diagnostics.rs` covers the report line, for both a build that can connect and
one that cannot.

## Open questions and gates

- **Which share, and who may read it.** The share's permissions are the access
  control for an unencrypted file holding personal data and the record of who
  carries which security token. Which share is acceptable, and what its ACL must
  be, is an **ESI** decision that this feature makes possible, not one it makes.
- **Whether a named account may be used at all.** Typing a service account's
  password into a desktop application is a pattern ESI may want to forbid in
  favour of the signed-in user plus a share ACL. The mechanism exists because the
  NAS case needs it; the policy is not the implementer's.
- **Domain authentication.** Reaching a share as the signed-in domain user is
  ordinary file access, not an AD integration — but if this ever grows to
  *selecting* a domain controller or requesting a Kerberos ticket, that is the
  ESI gate in AGENTS.md §8, and phase 10 stays Todo until it is asked for.
- **Guest access** is off by every default and has to be chosen. Whether a
  guest-readable share may hold this register at all is, again, ESI's.

## References

- `src/store/smb/mod.rs`, `src/store/smb/windows.rs`, `src/store/smb/macos.rs`,
  `src/store/smb/unsupported.rs`, `src/store/smb/mock.rs`
- `features/storage-sqlite-single-file.md`, `features/cloud-sync-hosting.md`,
  `features/database-selection.md`, `docs/operations.md`
- Microsoft: [`WNetAddConnection2W`](https://learn.microsoft.com/windows/win32/api/winnetwk/nf-winnetwk-wnetaddconnection2w)
- Apple: `NetFS.framework`, `NetFSMountURLSync`
- SQLite: [WAL and network filesystems](https://sqlite.org/wal.html)
