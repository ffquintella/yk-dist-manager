//! **Manual** hardware verification of the PIV write path.
//!
//! This one carries more weight than its FIDO2 counterpart. Every mutating call
//! it makes is behind the `yubikey` crate's `untested` feature — upstream's own
//! name for code they have not exercised — and the decision to enable that was
//! taken on the understanding that *we* would exercise it. This is that
//! exercise. See `features/step-piv-pin-puk-management-key.md`.
//!
//! ## What it does to the key
//!
//! Changes the PIN and PUK, replaces the management key with a random
//! PIN-protected one, and generates a signing key in slot 9c. Undone by
//! `ykman piv reset`, which restores the factory PIN, PUK and management key and
//! **destroys anything in the PIV slots**. Do not run this against a key anybody
//! uses.
//!
//! Unlike the FIDO2 reset, `ykman piv reset` needs no re-insertion — but it does
//! require the PIN and PUK to be blocked first, so the script blocks them
//! deliberately at the end.
//!
//! ```bash
//! cargo run --release --features native-piv --example verify_piv_write -- 36668917
//! ykman piv reset --force
//! ```
//!
//! Secrets are **generated**, never hard-coded: a credential in the repository
//! is forbidden regardless of purpose, and generating them exercises the same
//! `crate::secret` path a real run uses.

#[cfg(not(feature = "native-piv"))]
fn main() {
    eprintln!("rebuild with `--features native-piv`: this procedure needs the PC/SC transport");
    std::process::exit(2);
}

#[cfg(feature = "native-piv")]
fn main() {
    use yk_dist_manager::device::native_piv::NativePiv;
    use yk_dist_manager::device::write::PivWriter;
    use yk_dist_manager::secret::{Secret, SecretKind};

    let Some(serial) = std::env::args().nth(1).and_then(|a| a.parse::<u32>().ok()) else {
        eprintln!(
            "usage: verify_piv_write <serial>\n\n\
             The serial is required so this cannot run by accident. It WILL change the\n\
             PIN, PUK and management key and generate a key in slot 9c. Only\n\
             `ykman piv reset` undoes it, and that destroys the PIV slots."
        );
        std::process::exit(2);
    };

    println!("== PIV write verification against serial {serial} ==");
    println!("   (every write below is behind yubikey/untested — that is the point)\n");
    let mut piv = NativePiv::for_key(serial);

    // ---------------------------------------------------------------- before
    let before = piv.piv_state(serial).expect("read the applet state");
    println!("before: {before:?}");
    if before.pin_changed_from_default() || before.management_key_changed() {
        eprintln!(
            "\nREFUSING: this key's PIV applet is already configured.\n\
             This procedure is for a factory-fresh applet — run `ykman piv reset --force` first."
        );
        std::process::exit(1);
    }

    let pin = Secret::generate(SecretKind::PivPin, 0).expect("generate a PIV PIN");
    let puk = Secret::generate(SecretKind::PivPuk, 0).expect("generate a PUK");
    let mgm = Secret::generate(SecretKind::PivManagementKey, 0).expect("generate a management key");

    // ------------------------------------------------------- PIN and PUK
    println!("1. set_pin_and_puk  (from the factory defaults)");
    match piv.set_pin_and_puk(serial, None, &pin, None, &puk) {
        Ok(()) => {
            let state = piv.piv_state(serial).expect("read back");
            println!(
                "   -> pin_changed_from_default={} {}",
                state.pin_changed_from_default(),
                if state.pin_changed_from_default() {
                    "OK"
                } else {
                    "NOT REPORTED — the metadata read disagrees with the write"
                }
            );
        }
        Err(e) => {
            println!("   -> FAILED: {e}");
            std::process::exit(1);
        }
    }

    // ------------------------------------------------- management key
    // The one whose failure mode is worst: set to a value nobody holds, the
    // applet is administratively dead. `protect = true` puts it on the card
    // under the PIN, so there is nothing to hold.
    println!("\n2. set_management_key(protect = true)  <- the dangerous one");
    match piv.set_management_key(serial, None, &mgm, true, &pin) {
        Ok(()) => {
            let state = piv.piv_state(serial).expect("read back");
            println!(
                "   -> management_key_changed={} {}",
                state.management_key_changed(),
                if state.management_key_changed() {
                    "OK — and PIN-protected, so nothing needs custody"
                } else {
                    "NOT REPORTED — investigate before relying on it"
                }
            );
        }
        Err(e) => {
            println!("   -> FAILED: {e}");
            println!("      The applet may now be in an unknown state. Run `ykman piv reset`.");
            std::process::exit(1);
        }
    }

    // ------------------------------------------------------- key generation
    println!("\n3. generate_key(9c, ECCP256)  — on-device, never imported");
    match piv.generate_key(serial, "9c", "ECCP256", &pin) {
        Ok(evidence) => {
            println!(
                "   -> {} key generated in slot {}",
                evidence.algorithm, evidence.slot
            );
            let first = evidence
                .public_key_pem
                .lines()
                .nth(1)
                .unwrap_or("<no body>");
            println!("      public key begins {first}");
        }
        Err(e) => println!("   -> FAILED: {e}"),
    }

    // ------------------------------------------------------------- the CSR
    // Expected to refuse: the crate has no PKCS#10 builder, and a request
    // without the rfc822Name SAN would be silently useless.
    println!("\n4. create_csr  — expected to refuse, and to say why");
    match piv.create_csr(serial, "9c", "CN=Verification", "verify@example.org", &pin) {
        Ok(_) => println!("   -> UNEXPECTED: a CSR was produced"),
        Err(e) => println!("   -> refused as designed: {e}"),
    }

    // ----------------------------------------------------------------- after
    let after = piv.piv_state(serial).expect("read the applet state");
    println!("\nafter: {after:?}");

    println!(
        "\n== done ==\n\n\
         The PIV applet now has a PIN, PUK and management key this process generated\n\
         and did not keep, plus a key in slot 9c. Put it back with:\n\
         \n    ykman piv reset --force\n"
    );
}
