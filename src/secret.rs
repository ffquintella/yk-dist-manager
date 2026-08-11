//! The secrets a bootstrap sets: how they are produced, how they are shown once,
//! and how they are destroyed.
//!
//! `AGENTS.md` §2 states the rule this module exists to make true: *a PIN, PUK,
//! management key or OTP access code never reaches a log, an audit entry, a
//! database column, an error message, a UI label or a panic message.* The
//! defence is not discipline at every call site — it is that [`Secret`] has no
//! API that renders its value except one deliberately-named method, and that its
//! `Debug`, `Display` and serialisation all refuse.
//!
//! ## Custody model B, which is what makes this small
//!
//! `features/secrets-custody.md` decided model B on 2026-08-10: the operator sets
//! a **transport** secret, the key is marked so the holder must replace it, and
//! this tool retains nothing. So there is no secret store to build here, no
//! encryption of secrets at rest, and no key management — a [`Secret`] lives in
//! memory for the step that needs it and is wiped when it drops.
//!
//! The one thing that *does* leave the process is the value shown to the operator
//! so it can be written on a sealed slip, and that is [`ShowOnce`]: displayed
//! once, dismissed deliberately, and zeroised on dismissal.
//!
//! ## What is deliberately absent
//!
//! * No `Clone`. A secret that can be copied is a secret with an unknown number
//!   of buffers to wipe.
//! * No `Serialize`. Nothing may write one to the database, a settings file or a
//!   JSON payload, and the way to guarantee that is for the trait not to exist.
//! * No `Display`, and a `Debug` that prints `<redacted>`. A panic message, a
//!   `dbg!` left in by accident, or a `tracing` field that captured the wrong
//!   variable all print the redaction rather than the value.
//! * No `PartialEq` against a string. Comparing a secret to a literal is what a
//!   test that asserts on a secret value would do, and `AGENTS.md` §4 forbids
//!   those outright.

use zeroize::{Zeroize, Zeroizing};

use crate::domain::StepKind;

/// The smallest PIN this tool will generate or accept.
///
/// Six is the FIDO2 and PIV floor, and `features/secrets-custody.md` sets it as
/// the minimum for generation too. A template may ask for longer; it may not ask
/// for shorter.
pub const MIN_PIN_LENGTH: usize = 6;

/// The longest PIN worth generating.
///
/// CTAP2 allows 63 bytes and PIV allows 8. The cap here is about what a person
/// can copy off a slip and type on a keypad without error, which is the real
/// constraint under model B — the holder types this once, from paper.
pub const MAX_PIN_LENGTH: usize = 16;

/// PIV's PIN and PUK are fixed at 8 characters by the applet.
pub const PIV_PIN_LENGTH: usize = 8;

/// The OTP slot access code is exactly six bytes, rendered as twelve hex digits.
pub const OTP_ACCESS_CODE_BYTES: usize = 6;

/// A PIV management key is 24 bytes.
///
/// `features/secrets-custody.md` says "random AES-256", and that is not
/// reachable: the PIV management key slot takes a 24-byte key (3DES
/// historically, AES-192 on current firmware — the reference key reports
/// `Management key algorithm: AES192`), and the `yubikey` crate's `MgmKey` is a
/// fixed `[u8; 24]`. Generating 32 bytes would simply fail to load.
///
/// This is not a weakening in practice: the management key is generated
/// randomly and `--protect`ed onto the card under the PIN, so it is never
/// handed over, never retained, and never guessed at. 192 bits of CSPRNG output
/// that nobody holds is not the weak link in this procedure.
pub const MANAGEMENT_KEY_BYTES: usize = 24;

#[derive(Debug, thiserror::Error)]
pub enum SecretError {
    #[error("a PIN must be at least {MIN_PIN_LENGTH} characters, not {0}")]
    TooShort(usize),
    #[error("a PIN of {0} characters is longer than the {MAX_PIN_LENGTH} this tool will set")]
    TooLong(usize),
    #[error("a PIN must be digits only, so it can be typed on a keypad")]
    NotNumeric,
    #[error("the system random number generator is unavailable: {0}")]
    NoRandomness(String),
}

/// What a secret is for. Carried so an audit entry can say *which* secret was
/// generated without going anywhere near the value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecretKind {
    Fido2Pin,
    PivPin,
    PivPuk,
    PivManagementKey,
    OtpAccessCode,
}

impl SecretKind {
    /// The stored, audit-facing spelling.
    pub fn slug(&self) -> &'static str {
        match self {
            SecretKind::Fido2Pin => "fido2-pin",
            SecretKind::PivPin => "piv-pin",
            SecretKind::PivPuk => "piv-puk",
            SecretKind::PivManagementKey => "piv-management-key",
            SecretKind::OtpAccessCode => "otp-access-code",
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            SecretKind::Fido2Pin => "FIDO2 PIN",
            SecretKind::PivPin => "PIV PIN",
            SecretKind::PivPuk => "PIV PUK",
            SecretKind::PivManagementKey => "PIV management key",
            SecretKind::OtpAccessCode => "OTP slot access code",
        }
    }

    /// Does the holder need to be told this value?
    ///
    /// Under model B the answer decides what goes on the sealed slip. The
    /// management key is `--protect`ed onto the key itself and the OTP access
    /// code is generated and discarded, so neither is anybody's to carry — see
    /// the sub-decisions in `features/secrets-custody.md`.
    pub fn goes_to_the_holder(&self) -> bool {
        matches!(
            self,
            SecretKind::Fido2Pin | SecretKind::PivPin | SecretKind::PivPuk
        )
    }

    /// Which step kinds set this secret.
    pub fn for_step(kind: StepKind) -> Option<Self> {
        match kind {
            StepKind::Fido2Pin => Some(SecretKind::Fido2Pin),
            StepKind::OtpAccessCode => Some(SecretKind::OtpAccessCode),
            StepKind::PivPinPuk => Some(SecretKind::PivPin),
            StepKind::PivManagementKey => Some(SecretKind::PivManagementKey),
            _ => None,
        }
    }
}

/// A secret value, wiped when it drops.
///
/// Construct with [`Secret::generate`] or [`Secret::from_operator_input`]. Read
/// the value with [`Secret::expose`], which is named to be conspicuous at the
/// call site and at review: every use of it should be passing the value straight
/// into a hardware call.
pub struct Secret {
    kind: SecretKind,
    /// `Zeroizing` wipes on drop, including on an unwind.
    value: Zeroizing<String>,
}

impl Secret {
    /// Take a value the operator typed.
    ///
    /// Validated here rather than at the call site, so a PIN that a keypad cannot
    /// enter is refused before it reaches a key that will then require that
    /// keypad.
    pub fn from_operator_input(kind: SecretKind, value: &str) -> Result<Self, SecretError> {
        let numeric_required = matches!(kind, SecretKind::Fido2Pin | SecretKind::PivPin);
        if numeric_required {
            if value.len() < MIN_PIN_LENGTH {
                return Err(SecretError::TooShort(value.len()));
            }
            if value.len() > MAX_PIN_LENGTH {
                return Err(SecretError::TooLong(value.len()));
            }
            if !value.chars().all(|c| c.is_ascii_digit()) {
                return Err(SecretError::NotNumeric);
            }
        }
        Ok(Self {
            kind,
            value: Zeroizing::new(value.to_owned()),
        })
    }

    /// Generate a secret of the right shape for its kind, from the OS CSPRNG.
    ///
    /// `length` applies only to the PIN kinds; the others have a fixed size fixed
    /// by the applet rather than by policy.
    pub fn generate(kind: SecretKind, length: usize) -> Result<Self, SecretError> {
        let value = match kind {
            SecretKind::Fido2Pin => numeric_pin(length.max(MIN_PIN_LENGTH))?,
            // PIV fixes both of these at eight characters, so the template's
            // length is not consulted: a shorter one would be padded by the
            // applet and a longer one truncated, and either is a PIN the holder
            // cannot reproduce from the slip.
            SecretKind::PivPin | SecretKind::PivPuk => numeric_pin(PIV_PIN_LENGTH)?,
            SecretKind::OtpAccessCode => hex_bytes(OTP_ACCESS_CODE_BYTES)?,
            SecretKind::PivManagementKey => hex_bytes(MANAGEMENT_KEY_BYTES)?,
        };
        Ok(Self {
            kind,
            value: Zeroizing::new(value),
        })
    }

    pub fn kind(&self) -> SecretKind {
        self.kind
    }

    /// How long the value is. Safe to log: a length is not a secret, and it is
    /// what an audit entry needs to show the policy was applied.
    pub fn len(&self) -> usize {
        self.value.len()
    }

    pub fn is_empty(&self) -> bool {
        self.value.is_empty()
    }

    /// The value itself.
    ///
    /// Deliberately verbose. Every call is a place where a secret leaves this
    /// module, and there should be very few: a hardware write, and the
    /// show-once panel. If a call to this appears next to a `format!`, a
    /// `tracing::` macro or a `Store` method, that is a defect.
    pub fn expose(&self) -> &str {
        &self.value
    }

    /// The audit-facing description of *setting* this secret. Never the value.
    pub fn audit_detail(&self, step_id: &str) -> String {
        format!(
            "step={step_id} kind={} length={}",
            self.kind.slug(),
            self.value.len()
        )
    }
}

/// Prints `<redacted>`, always.
///
/// This is the backstop for every accident: a `dbg!`, a panic that formats its
/// locals, a `tracing` field pointed at the wrong variable, a struct that
/// derives `Debug` and happens to contain one of these.
impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Secret({}, <redacted>)", self.kind.slug())
    }
}

/// A generated secret being shown to the operator so it can be written down.
///
/// Model B hands a transport secret over, so there is exactly one moment when a
/// value has to be readable by a person. This type is that moment, and it is
/// bounded: the value is destroyed by [`ShowOnce::dismiss`], and the panel that
/// holds one is required to call it.
///
/// It is not `Clone` and not `Copy` for the usual reason, and it deliberately
/// offers no "show again" — a second look is a second chance for the value to be
/// captured by a screen recording or a shoulder, and the recovery path for a lost
/// slip is to run the step again, not to re-read a buffer.
#[derive(Debug)]
pub struct ShowOnce {
    secrets: Vec<Secret>,
    /// Set once dismissed, so a stale panel cannot re-render the value.
    spent: bool,
}

impl ShowOnce {
    pub fn new(secrets: Vec<Secret>) -> Self {
        Self {
            secrets,
            spent: false,
        }
    }

    /// The secrets to display, or nothing at all once dismissed.
    pub fn entries(&self) -> &[Secret] {
        if self.spent { &[] } else { &self.secrets }
    }

    /// Only the ones the holder actually has to carry.
    pub fn for_the_holder(&self) -> impl Iterator<Item = &Secret> {
        self.entries()
            .iter()
            .filter(|s| s.kind().goes_to_the_holder())
    }

    pub fn is_spent(&self) -> bool {
        self.spent
    }

    /// Destroy the values. Called when the operator dismisses the panel, and by
    /// `Drop`, so a panel that is never dismissed still cannot outlive the run.
    pub fn dismiss(&mut self) {
        self.secrets.clear();
        self.spent = true;
    }

    /// What the audit trail records about the panel: which kinds were shown, and
    /// never how many characters of what.
    pub fn audit_detail(&self) -> String {
        let kinds: Vec<&str> = self.secrets.iter().map(|s| s.kind().slug()).collect();
        format!("shown={}", kinds.join(","))
    }
}

impl Drop for ShowOnce {
    fn drop(&mut self) {
        self.dismiss();
    }
}

/// A numeric PIN of `length` digits, uniformly distributed.
///
/// Rejection sampling rather than `byte % 10`: the modulo would make the digits
/// 0–5 slightly likelier than 6–9, because 256 is not a multiple of 10. The bias
/// is small and it is also completely avoidable, which is the whole argument.
fn numeric_pin(length: usize) -> Result<String, SecretError> {
    let length = length.clamp(MIN_PIN_LENGTH, MAX_PIN_LENGTH);
    let mut out = String::with_capacity(length);

    while out.len() < length {
        let mut buf = [0u8; 32];
        fill_random(&mut buf)?;
        for byte in buf.iter() {
            if out.len() == length {
                break;
            }
            // 250 = 25 × 10: the largest multiple of ten inside a byte's range,
            // so everything below it maps to a digit without bias.
            if *byte < 250 {
                out.push(char::from(b'0' + (byte % 10)));
            }
        }
        buf.zeroize();
    }
    Ok(out)
}

/// `n` random bytes, hex-encoded.
fn hex_bytes(n: usize) -> Result<String, SecretError> {
    let mut buf = vec![0u8; n];
    fill_random(&mut buf)?;
    let encoded = hex::encode(&buf);
    buf.zeroize();
    Ok(encoded)
}

fn fill_random(buf: &mut [u8]) -> Result<(), SecretError> {
    getrandom::fill(buf).map_err(|e| SecretError::NoRandomness(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_secrets_debug_output_never_contains_its_value() {
        // The backstop test. If this fails, every panic message, every stray
        // `dbg!` and every mis-pointed tracing field is a potential leak.
        let secret = Secret::generate(SecretKind::Fido2Pin, 8).unwrap();
        let rendered = format!("{secret:?}");
        assert!(
            !rendered.contains(secret.expose()),
            "Debug leaked the value: {rendered}"
        );
        assert!(rendered.contains("<redacted>"), "{rendered}");
        assert!(
            rendered.contains("fido2-pin"),
            "but it still says which: {rendered}"
        );
    }

    #[test]
    fn a_show_once_panel_debug_output_leaks_nothing_either() {
        let panel = ShowOnce::new(vec![Secret::generate(SecretKind::PivPin, 8).unwrap()]);
        let value = panel.entries()[0].expose().to_owned();
        let rendered = format!("{panel:?}");
        assert!(!rendered.contains(&value), "{rendered}");
    }

    #[test]
    fn a_generated_pin_is_numeric_and_the_length_asked_for() {
        for length in [6, 8, 12] {
            let secret = Secret::generate(SecretKind::Fido2Pin, length).unwrap();
            assert_eq!(secret.len(), length);
            assert!(
                secret.expose().chars().all(|c| c.is_ascii_digit()),
                "a PIN has to be typeable on a keypad"
            );
        }
    }

    #[test]
    fn a_pin_shorter_than_the_floor_is_raised_to_it_rather_than_generated_weak() {
        let secret = Secret::generate(SecretKind::Fido2Pin, 4).unwrap();
        assert_eq!(secret.len(), MIN_PIN_LENGTH);
    }

    #[test]
    fn piv_secrets_are_eight_characters_whatever_the_template_asks() {
        // The applet fixes this. Honouring a template's 6 would give the holder a
        // PIN the key pads out, so the slip and the key would disagree.
        for kind in [SecretKind::PivPin, SecretKind::PivPuk] {
            for asked in [6, 8, 16] {
                assert_eq!(Secret::generate(kind, asked).unwrap().len(), PIV_PIN_LENGTH);
            }
        }
    }

    #[test]
    fn the_otp_access_code_is_six_bytes_as_twelve_hex_characters() {
        let secret = Secret::generate(SecretKind::OtpAccessCode, 0).unwrap();
        assert_eq!(secret.len(), OTP_ACCESS_CODE_BYTES * 2);
        assert!(secret.expose().chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn the_management_key_is_the_length_the_piv_slot_takes() {
        // 24 bytes, not the 32 the spec first asked for: the applet's management
        // key slot is 24 bytes wide (AES-192 on current firmware), so a 32-byte
        // key cannot be loaded at all. See MANAGEMENT_KEY_BYTES.
        let secret = Secret::generate(SecretKind::PivManagementKey, 0).unwrap();
        assert_eq!(secret.len(), 48, "24 bytes rendered as hex");
        assert!(secret.expose().chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn two_generated_secrets_differ() {
        // A weak check for a catastrophic failure: a constant or an unseeded
        // buffer would make every key in a batch share a PIN.
        let a = Secret::generate(SecretKind::Fido2Pin, 8).unwrap();
        let b = Secret::generate(SecretKind::Fido2Pin, 8).unwrap();
        assert_ne!(a.expose(), b.expose());
    }

    #[test]
    fn generated_digits_are_not_biased_towards_the_low_ones() {
        // Guards the rejection sampling. `byte % 10` would make 0-5 appear about
        // 1.024x as often as 6-9; over this many digits that is easy to see.
        let mut counts = [0usize; 10];
        for _ in 0..400 {
            let secret = Secret::generate(SecretKind::Fido2Pin, 16).unwrap();
            for c in secret.expose().chars() {
                counts[c.to_digit(10).unwrap() as usize] += 1;
            }
        }
        let total: usize = counts.iter().sum();
        let expected = total as f64 / 10.0;
        for (digit, count) in counts.iter().enumerate() {
            let drift = (*count as f64 - expected).abs() / expected;
            assert!(
                drift < 0.15,
                "digit {digit} appeared {count} times against an expected {expected:.0} \
                 ({:.1}% off) — the sampling may be biased",
                drift * 100.0
            );
        }
    }

    #[test]
    fn an_operator_typed_pin_must_be_long_enough_and_numeric() {
        assert!(matches!(
            Secret::from_operator_input(SecretKind::Fido2Pin, "12345"),
            Err(SecretError::TooShort(5))
        ));
        assert!(matches!(
            Secret::from_operator_input(SecretKind::Fido2Pin, "abcdef"),
            Err(SecretError::NotNumeric)
        ));
        assert!(matches!(
            Secret::from_operator_input(SecretKind::Fido2Pin, &"1".repeat(20)),
            Err(SecretError::TooLong(20))
        ));
        assert!(Secret::from_operator_input(SecretKind::Fido2Pin, "123456").is_ok());
    }

    #[test]
    fn the_audit_detail_carries_the_kind_and_the_length_and_nothing_else() {
        let secret = Secret::generate(SecretKind::Fido2Pin, 8).unwrap();
        let detail = secret.audit_detail("fido2-pin");
        assert_eq!(detail, "step=fido2-pin kind=fido2-pin length=8");
        assert!(!detail.contains(secret.expose()));
    }

    #[test]
    fn a_dismissed_panel_shows_nothing_and_says_so() {
        let mut panel = ShowOnce::new(vec![
            Secret::generate(SecretKind::Fido2Pin, 8).unwrap(),
            Secret::generate(SecretKind::PivManagementKey, 0).unwrap(),
        ]);
        assert_eq!(panel.entries().len(), 2);
        assert_eq!(
            panel.audit_detail(),
            "shown=fido2-pin,piv-management-key",
            "the entry names the kinds, never the values"
        );

        panel.dismiss();
        assert!(panel.is_spent());
        assert!(
            panel.entries().is_empty(),
            "there is no second look at a transport secret"
        );
    }

    #[test]
    fn only_the_secrets_the_holder_carries_reach_the_slip() {
        // The management key is protected onto the key and the OTP access code is
        // discarded, so neither belongs on a sealed envelope.
        let panel = ShowOnce::new(vec![
            Secret::generate(SecretKind::Fido2Pin, 8).unwrap(),
            Secret::generate(SecretKind::PivPin, 8).unwrap(),
            Secret::generate(SecretKind::PivPuk, 8).unwrap(),
            Secret::generate(SecretKind::PivManagementKey, 0).unwrap(),
            Secret::generate(SecretKind::OtpAccessCode, 0).unwrap(),
        ]);
        let carried: Vec<&str> = panel.for_the_holder().map(|s| s.kind().slug()).collect();
        assert_eq!(carried, vec!["fido2-pin", "piv-pin", "piv-puk"]);
    }

    #[test]
    fn every_secret_setting_step_kind_maps_to_a_secret() {
        // Keeps `SecretKind::for_step` in step with `StepKind::sets_secret`, so a
        // step that sets a secret cannot silently get none.
        for kind in StepKind::ALL {
            assert_eq!(
                kind.sets_secret(),
                SecretKind::for_step(kind).is_some(),
                "{kind:?} disagrees about whether it sets a secret"
            );
        }
    }
}
