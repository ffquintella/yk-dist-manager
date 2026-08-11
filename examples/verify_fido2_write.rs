//! **Manual** hardware verification of the FIDO2 write path.
//!
//! `AGENTS.md` §4 is absolute: *no test writes to a YubiKey, ever, not even an
//! ignored one.* So this is not a test. It is the manual procedure that
//! `features/testing-strategy.md` requires instead — run deliberately, by an
//! operator, against a key they have decided is expendable — and its output is
//! what gets pasted into the phase notes as evidence.
//!
//! ## What it does to the key
//!
//! Sets a FIDO2 PIN, raises the minimum PIN length, sets `forcePINChange`, and
//! creates a resident credential. **None of that is reversible** except by
//! `ykman fido reset`, which destroys every credential on the key. Do not run
//! this against a key anybody uses.
//!
//! ## Running it
//!
//! The serial must be repeated on the command line, so that running the example
//! by itself does nothing:
//!
//! ```bash
//! cargo run --release --features native-fido --example verify_fido2_write -- 36668917
//! # then, to put the key back:
//! ykman fido reset
//! ```
//!
//! The PIN is **generated**, never hard-coded: a credential in the repository is
//! forbidden regardless of what it is for, and generating it also exercises the
//! same `crate::secret` path a real run uses.

#[cfg(not(feature = "native-fido"))]
fn main() {
    eprintln!("rebuild with `--features native-fido`: this procedure needs the CTAP transport");
    std::process::exit(2);
}

#[cfg(feature = "native-fido")]
fn main() {
    use yk_dist_manager::device::native_fido::NativeFido2;
    use yk_dist_manager::device::write::{CredentialRequest, Fido2Writer};
    use yk_dist_manager::secret::{Secret, SecretKind};

    let Some(serial) = std::env::args().nth(1).and_then(|a| a.parse::<u32>().ok()) else {
        eprintln!(
            "usage: verify_fido2_write <serial>\n\n\
             The serial is required so this cannot run by accident. It WILL set a PIN\n\
             and create a credential on that key, and only `ykman fido reset` undoes it."
        );
        std::process::exit(2);
    };

    println!("== FIDO2 write verification against serial {serial} ==\n");
    let mut fido = NativeFido2::for_key(serial);

    // ---------------------------------------------------------------- before
    let before = fido.fido2_state(serial).expect("read the applet state");
    println!("before: {before:?}");
    if before.pin_set {
        eprintln!(
            "\nREFUSING: this key already has a FIDO2 PIN.\n\
             This procedure is for a factory-fresh applet — run `ykman fido reset` first."
        );
        std::process::exit(1);
    }

    // ------------------------------------------------------------- set a PIN
    let pin = Secret::generate(SecretKind::Fido2Pin, 8).expect("generate a transport PIN");
    println!("\n1. set_pin (length {})", pin.len());
    fido.set_pin(serial, &pin).expect("set_pin");
    let state = fido.fido2_state(serial).expect("read back");
    assert!(state.pin_set, "the key should now report a PIN is set");
    println!("   -> pin_set={} OK", state.pin_set);

    // ------------------------------------------------- minimum PIN length
    // CTAP 2.1, firmware 5.7+. On an older key this is expected to refuse, and
    // the refusal is the result rather than a failure of the procedure.
    println!("\n2. set_min_pin_length(8)");
    match fido.set_min_pin_length(serial, 8, &pin) {
        Ok(()) => {
            let state = fido.fido2_state(serial).expect("read back");
            println!("   -> min_pin_length={:?} OK", state.min_pin_length);
        }
        Err(e) => println!("   -> refused: {e}\n      (expected below firmware 5.7)"),
    }

    // ------------------------------------------------------ forcePINChange
    // The mechanism custody model B depends on. This is the step that decides
    // whether the enforcement is `enforced-by-firmware` or only
    // `instructed-on-handover`.
    println!("\n3. force_pin_change  <- the model B enforcement");
    match fido.force_pin_change(serial, &pin) {
        Ok(()) => {
            let state = fido.fido2_state(serial).expect("read back");
            println!(
                "   -> force_pin_change_set={} {}",
                state.force_pin_change_set,
                if state.force_pin_change_set {
                    "OK — enforcement is enforced-by-firmware on this key"
                } else {
                    "SET BUT NOT REPORTED — investigate before relying on it"
                }
            );
        }
        Err(e) => println!("   -> refused: {e}\n      (expected below firmware 5.7)"),
    }

    // ------------------------------------------------- resident credential
    // The step `ykman` cannot perform at all, and therefore the one this whole
    // transport exists for.
    println!("\n4. make_credential (resident)  — TOUCH THE KEY WHEN IT BLINKS");
    let request = CredentialRequest {
        relying_party: "example.org".into(),
        relying_party_name: "Example Org".into(),
        user_name: "verification@example.org".into(),
        user_display_name: "Hardware verification".into(),
        resident: true,
        require_user_verification: true,
    };
    match fido.make_credential(serial, &request, &pin) {
        Ok(evidence) => println!(
            "   -> credential {} for {} ({}) OK",
            evidence.credential_id_hex, evidence.relying_party, evidence.algorithm
        ),
        Err(e) => println!("   -> FAILED: {e}"),
    }

    // ----------------------------------------------------------------- after
    let after = fido.fido2_state(serial).expect("read the applet state");
    println!("\nafter: {after:?}");

    println!(
        "\n== done ==\n\n\
         The key now has a PIN and a resident credential on it. Put it back with:\n\
         \n    ykman fido reset\n\n\
         The PIN this procedure generated was never printed and is now dropped, so\n\
         the reset is the only way back — which is the point of the exercise."
    );
}
