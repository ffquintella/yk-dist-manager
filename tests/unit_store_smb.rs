//! Unit tests for hosting the database on an SMB share (`store::smb`).
//!
//! No file server is involved anywhere: the connection is a [`Connector`], and
//! `MockConnector` is one that answers from a script and records what it was asked.
//! That is what makes the two rules that matter testable at all — a share the
//! operator had already mounted is never connected, and a connection this session
//! made is disconnected exactly once.
//!
//! The password is deliberately unreachable from here: `Secret::expose` is
//! crate-private, so a test cannot assert on a password, because no test should
//! have one to assert on. What is asserted is that it never comes *out*.

use std::path::{Path, PathBuf};

use yk_dist_manager::store::smb::{
    self, Access, Attachment, Credential, MockConnector, ShareConnection, SmbError,
    StubbornConnector,
};

fn target(location: &str) -> smb::ShareTarget {
    smb::parse(location).unwrap().target
}

// ------------------------------------------------------------------- the location

#[test]
fn every_spelling_of_a_share_reaches_the_same_place() {
    // An operator pastes what their platform gave them. All four forms are the
    // same share, and the tool must not care which one arrived.
    for raw in [
        "smb://fileserver/ti-share/yubikeys/keys.sqlite3",
        "cifs://fileserver/ti-share/yubikeys/keys.sqlite3",
        r"\\fileserver\ti-share\yubikeys\keys.sqlite3",
        "//fileserver/ti-share/yubikeys/keys.sqlite3",
    ] {
        let parsed = smb::parse(raw).unwrap_or_else(|e| panic!("{raw}: {e}"));
        assert_eq!(parsed.target.host, "fileserver", "{raw}");
        assert_eq!(parsed.target.share, "ti-share", "{raw}");
        assert_eq!(parsed.target.inner, "yubikeys/keys.sqlite3", "{raw}");
        assert_eq!(
            parsed.target.location(),
            "//fileserver/ti-share/yubikeys/keys.sqlite3",
            "the remembered form is one spelling, whichever was typed: {raw}"
        );
    }
}

#[test]
fn a_local_path_is_not_mistaken_for_a_share() {
    for raw in [
        "/Volumes/ti-share/keys.sqlite3",
        "C:\\keys.sqlite3",
        "keys.sqlite3",
        "~/keys.sqlite3",
    ] {
        assert!(!smb::looks_like_smb_location(raw), "{raw}");
        assert!(matches!(smb::parse(raw), Err(SmbError::NotAShare { .. })));
    }
    // …and a share is.
    for raw in ["smb://h/s", r"\\h\s", "//h/s", "  SMB://h/s  "] {
        assert!(smb::looks_like_smb_location(raw), "{raw}");
    }
}

#[test]
fn a_location_that_cannot_name_a_database_is_refused_rather_than_guessed() {
    // No share at all: there is nothing to guess, and guessing would connect to
    // something the operator did not name.
    assert!(matches!(
        smb::parse("smb://fileserver"),
        Err(SmbError::NoShare { .. })
    ));
    // A traversal would put the register outside the share that was named.
    let escape = smb::parse(r"\\fileserver\ti-share\..\..\secrets\keys.sqlite3").unwrap_err();
    assert!(escape.to_string().contains("outside the share"), "{escape}");
    // And a control character never reaches a system call.
    assert!(smb::parse("smb://file\u{7}server/share").is_err());
}

#[test]
fn the_database_path_is_the_share_root_plus_the_path_inside_it() {
    let target = target("smb://nas/public/yubikeys/keys.sqlite3");
    assert_eq!(
        target.database_path(Path::new("/Volumes/public")),
        PathBuf::from("/Volumes/public/yubikeys/keys.sqlite3")
    );
    // On Windows the root *is* the UNC path.
    assert_eq!(
        target.database_path(Path::new(r"\\nas\public")),
        Path::new(r"\\nas\public")
            .join("yubikeys")
            .join("keys.sqlite3")
    );
}

// ---------------------------------------------------------------- the credential

#[test]
fn the_default_identity_is_the_operator_who_is_already_signed_in() {
    // This is the answer on Windows, where a UNC path is authenticated by the
    // session's own token — so it must also be what an operator gets without
    // choosing anything.
    assert_eq!(Access::default(), Access::LoggedOnUser);
    assert_eq!(Credential::default().access, Access::LoggedOnUser);
    assert_eq!(Access::ALL.len(), 3);
    assert_eq!(Access::ALL[0], Access::LoggedOnUser);
}

#[test]
fn nothing_that_describes_a_credential_can_carry_its_password() {
    let credential = Credential::named(r"FGV\felipe", "must-not-appear");

    // The three ways a value escapes a struct: a formatter, a description, and a
    // log field (which is `Display` or `Debug` on the value).
    assert!(!format!("{credential:?}").contains("must-not-appear"));
    assert!(format!("{credential:?}").contains("Secret(********)"));
    assert!(!credential.describe().contains("must-not-appear"));
    assert_eq!(credential.describe(), r"FGV\felipe");

    // And the identity of a passwordless mode says which mode it was.
    assert_eq!(Credential::anonymous().describe(), "guest");
    assert_eq!(
        Credential::logged_on_user().describe(),
        "the signed-in user"
    );
}

// ---------------------------------------------------------------- the connection

#[test]
fn a_share_the_operator_had_already_mounted_is_used_and_left_alone() {
    let connector = MockConnector::adopting("/Volumes/ti-share");
    let calls = connector.calls();
    let identities = connector.identities();

    let connection = ShareConnection::open(
        &target("smb://fileserver/ti-share/keys.sqlite3"),
        &Credential::named("felipe", "never-sent"),
        Box::new(connector),
    )
    .unwrap();

    assert!(!connection.is_ours());
    assert_eq!(
        connection.database_path(),
        PathBuf::from("/Volumes/ti-share/keys.sqlite3")
    );
    connection.close().unwrap();

    // The probe answered, so nothing was connected — and no credential was ever
    // presented to a server that did not need one.
    assert_eq!(calls.lock().unwrap().as_slice(), ["existing_root"]);
    assert!(identities.lock().unwrap().is_empty());
}

#[test]
fn a_share_this_session_connected_is_disconnected_exactly_once() {
    let connector = MockConnector::connecting("/Volumes/ti-share");
    let calls = connector.calls();

    let connection = ShareConnection::open(
        &target("smb://fileserver/ti-share/keys.sqlite3"),
        &Credential::anonymous(),
        Box::new(connector),
    )
    .unwrap();
    assert!(connection.is_ours());
    connection.close().unwrap();

    // Closing disconnects, and the drop that follows must not do it again: a second
    // disconnection would take down a share the *next* session had just mounted.
    assert_eq!(
        calls.lock().unwrap().as_slice(),
        ["existing_root", "connect", "disconnect"]
    );
}

#[test]
fn dropping_a_connection_still_frees_the_share() {
    let connector = MockConnector::connecting("/Volumes/ti-share");
    let calls = connector.calls();
    {
        let _connection = ShareConnection::open(
            &target("smb://fileserver/ti-share/keys.sqlite3"),
            &Credential::anonymous(),
            Box::new(connector),
        )
        .unwrap();
        assert!(!calls.lock().unwrap().contains(&"disconnect".to_owned()));
    }
    assert!(
        calls.lock().unwrap().contains(&"disconnect".to_owned()),
        "a panic or an early return must not leave the share attached"
    );
}

#[test]
fn the_identity_actually_presented_is_the_one_the_operator_chose() {
    // The hazard this guards: silently trying the signed-in user first under a
    // named account would open the register as an identity nobody reviewed — and
    // on a share that is read-only for everyone else, that looks like lost writes.
    for (credential, expected) in [
        (Credential::named("svc-yubikey", "x"), "svc-yubikey"),
        (Credential::anonymous(), "guest"),
        (Credential::logged_on_user(), "the signed-in user"),
    ] {
        let connector = MockConnector::connecting("/Volumes/ti-share");
        let identities = connector.identities();
        let connection = ShareConnection::open(
            &target("smb://fileserver/ti-share/keys.sqlite3"),
            &credential,
            Box::new(connector),
        )
        .unwrap();
        assert_eq!(identities.lock().unwrap().as_slice(), [expected.to_owned()]);
        assert!(connection.describe().contains(expected));
    }
}

#[test]
fn a_refused_credential_is_reported_with_the_share_and_attaches_nothing() {
    let connector = MockConnector::refusing("the user name or password was refused");
    let calls = connector.calls();

    let error = ShareConnection::open(
        &target("smb://fileserver/ti-share/keys.sqlite3"),
        &Credential::named("felipe", "wrong"),
        Box::new(connector),
    )
    .unwrap_err();

    let message = error.to_string();
    assert!(message.contains("//fileserver/ti-share"), "{message}");
    assert!(message.contains("felipe"), "{message}");
    assert!(message.contains("user name or password"), "{message}");
    assert!(
        !message.contains("wrong"),
        "no password in a message: {message}"
    );
    assert!(!calls.lock().unwrap().contains(&"disconnect".to_owned()));
}

#[test]
fn an_unreachable_server_is_told_apart_from_a_refused_credential() {
    let error = ShareConnection::open(
        &target("smb://fileserver/ti-share/keys.sqlite3"),
        &Credential::logged_on_user(),
        Box::new(MockConnector::unreachable("the server is not answering")),
    )
    .unwrap_err();
    // Two completely different things to go and do: fix the name or the network,
    // versus ask for access.
    assert!(matches!(error, SmbError::Unreachable { .. }), "{error}");
    assert!(
        error.to_string().contains("could not be reached"),
        "{error}"
    );
}

#[test]
fn a_disconnection_that_fails_is_reported_and_not_swallowed() {
    let dir = tempfile::tempdir().unwrap();
    let connection = ShareConnection::open(
        &target("smb://fileserver/ti-share/keys.sqlite3"),
        &Credential::anonymous(),
        Box::new(StubbornConnector {
            root: dir.path().to_path_buf(),
        }),
    )
    .unwrap();

    // The operator has to know the share is still attached: the next thing they do
    // may depend on it, and a silent failure here would look like a clean close.
    let error = connection.close().unwrap_err();
    assert!(matches!(error, SmbError::DetachFailed { .. }), "{error}");
    assert!(
        error.to_string().contains("//fileserver/ti-share"),
        "{error}"
    );
}

// ------------------------------------------------------- what this build can do

#[test]
fn a_platform_that_cannot_connect_says_so_rather_than_failing_quietly() {
    // Windows and macOS connect; anything else can only use a mount the system
    // made, and the chooser has to be able to say which of the two this is.
    assert_eq!(
        smb::can_connect(),
        cfg!(any(windows, target_os = "macos")),
        "the claim must match the build"
    );

    let connector = smb::platform_connector();
    assert!(!connector.label().is_empty());
    if !smb::can_connect() {
        let error = connector
            .connect(
                &target("smb://fileserver/ti-share"),
                &Credential::anonymous(),
            )
            .unwrap_err();
        assert!(matches!(error, SmbError::Unsupported { .. }), "{error}");
        assert!(error.to_string().contains("mount.cifs"), "{error}");
    }
}

#[test]
fn a_share_nobody_mounted_is_not_reported_as_reachable() {
    // The probe has to answer "no" for a share that is not there, on every
    // platform: a false positive would open the database at a path that does not
    // exist and report it as missing.
    assert_eq!(
        smb::platform_connector().existing_root(&target(
            "smb://no-such-server.invalid/no-such-share/keys.sqlite3"
        )),
        None
    );
}

#[test]
fn an_attachment_describes_itself_for_the_status_bar() {
    let attached = Attachment {
        target: target("smb://fileserver/ti-share/keys.sqlite3"),
        root: PathBuf::from("/Volumes/ti-share"),
        identity: r"FGV\felipe".into(),
        ours: true,
    };
    let line = attached.describe();
    assert!(line.contains("//fileserver/ti-share"), "{line}");
    assert!(line.contains(r"FGV\felipe"), "{line}");
    assert!(line.contains("connected by this application"), "{line}");
    assert_eq!(
        attached.database_path(),
        PathBuf::from("/Volumes/ti-share/keys.sqlite3")
    );

    let adopted = Attachment {
        ours: false,
        ..attached
    };
    assert!(adopted.describe().contains("already mounted"));
}
