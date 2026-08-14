//! The **management applet**: form factor, per-application enable flags, FIPS.
//!
//! `features/native-device-transport.md` phase 5, and the last read the native
//! transport was missing. Everything else about a key comes off an applet that
//! answers for itself — PIV knows its slots, FIDO2 knows its PIN — but *which
//! applications are enabled at all* is a property of the device, and it lives
//! here, behind CCID instruction `00 1D` on AID `A0 00 00 05 27 47 11 17`.
//!
//! ## Why this one gap mattered more than its size
//!
//! `device::native` left `usb_applications` empty, because it had no way to read
//! it. That empty list has caused the same class of bug twice, each time by a
//! reader deciding what silence meant:
//!
//! * it once reported `["PIV"]` — the applet it had just spoken to — and the
//!   pre-flight read that as *FIDO2 and OTP are disabled*, skipping five of the
//!   standard procedure's eleven steps on a key that had all three enabled;
//! * corrected to empty, the pre-flight then had to raise a warning on every
//!   applet-dependent step saying it could not check.
//!
//! Neither is wrong given what was available. Both go away once the field is
//! actually read, which is what this module does.
//!
//! ## Names, deliberately the same as `ykman`'s
//!
//! The application names produced here — `Yubico OTP`, `FIDO U2F`, `FIDO2`,
//! `OATH`, `PIV`, `OpenPGP`, `YubiHSM Auth` — are exactly the ones
//! [`super::ykman::parse_info`] produces from `ykman info`. That is not
//! cosmetic: [`crate::bootstrap::preflight`] matches these strings, so two
//! transports that named the same application differently would make a step skip
//! on one transport and run on the other.
//!
//! ## Not hardware-verified
//!
//! The parser is pure and covered by tests built from the encoding Yubico's own
//! `yubikit.management` documents, byte for byte. The **card exchange** around it
//! was written with no key attached, and says so — the same statement
//! `piv_session` carries, for the same reason.

use super::tlv::{be_integer, tlvs};

/// Management application id, selected over CCID.
#[cfg_attr(not(feature = "native-piv"), allow(dead_code))]
const MGMT_AID: [u8; 8] = [0xA0, 0x00, 0x00, 0x05, 0x27, 0x47, 0x11, 0x17];

/// `READ CONFIG`, the one instruction this module sends. A read: the applet's
/// write instruction is `0x1C`, which is deliberately not implemented here.
#[cfg_attr(not(feature = "native-piv"), allow(dead_code))]
const INS_READ_CONFIG: u8 = 0x1D;

const TAG_USB_SUPPORTED: u32 = 0x01;
const TAG_SERIAL: u32 = 0x02;
const TAG_USB_ENABLED: u32 = 0x03;
const TAG_FORM_FACTOR: u32 = 0x04;
const TAG_VERSION: u32 = 0x05;
const TAG_CONFIG_LOCK: u32 = 0x0A;
const TAG_NFC_SUPPORTED: u32 = 0x0D;
const TAG_NFC_ENABLED: u32 = 0x0E;
const TAG_MORE_DATA: u32 = 0x10;
const TAG_FIPS_CAPABLE: u32 = 0x14;
const TAG_FIPS_APPROVED: u32 = 0x15;

/// Capability bits, as the applet defines them. Not contiguous, and not in the
/// order they were introduced — which is why they are named rather than indexed.
const CAPABILITIES: [(u64, &str); 7] = [
    (0x0001, "Yubico OTP"),
    (0x0002, "FIDO U2F"),
    (0x0008, "OpenPGP"),
    (0x0010, "PIV"),
    (0x0020, "OATH"),
    (0x0100, "YubiHSM Auth"),
    (0x0200, "FIDO2"),
];

/// How many pages of device info this will ask for.
///
/// The applet says whether more is waiting (`TAG_MORE_DATA`), and a bound exists
/// because a card that always says "more" would otherwise loop for ever. Three is
/// past what any shipped firmware uses.
#[cfg_attr(not(feature = "native-piv"), allow(dead_code))]
const MAX_PAGES: u8 = 3;

/// What the management applet says about a key.
///
/// Every field is optional in the encoding, and stays optional here. A key whose
/// firmware predates a tag must read as *not said* rather than as zero: a zero
/// capability mask is the claim that nothing is enabled, and that claim skips
/// steps.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DeviceConfig {
    pub serial: Option<u32>,
    /// `5.7.4`, from the three version bytes.
    pub firmware: Option<String>,
    /// The operator-facing form factor, in `ykman`'s wording.
    pub form_factor: Option<String>,
    /// Applications enabled over USB, in `ykman`'s names.
    pub usb_enabled: Vec<String>,
    /// Applications the hardware has at all, whether enabled or not.
    pub usb_supported: Vec<String>,
    pub nfc_enabled: Vec<String>,
    pub nfc_supported: Vec<String>,
    /// True when the *device* is a FIPS model (the high bit of the form factor).
    pub fips_capable_device: bool,
    /// Applications that can be put in a FIPS-approved state (firmware 5.7+).
    pub fips_capable: Vec<String>,
    /// Applications currently *in* that state — which is the one that matters for
    /// a compliance claim, and is not implied by the one above.
    pub fips_approved: Vec<String>,
    /// A Security Key (the FIDO-only line), from the form factor's `0x40` bit.
    pub security_key: bool,
    /// The configuration is locked with a code, so nothing here can be changed
    /// without it. This tool never changes it; it is worth reporting because it
    /// explains why an operator cannot enable a missing application.
    pub config_locked: bool,
}

impl DeviceConfig {
    /// Does the key have NFC at all? Answered by the applet having said anything
    /// about NFC, which only a key that has it does.
    pub fn has_nfc(&self) -> bool {
        !self.nfc_supported.is_empty()
    }

    /// Is this application enabled over USB?
    ///
    /// `None` when the applet did not say, which is not `false`: see the type's
    /// own note. Callers that treat unknown as disabled reintroduce the bug this
    /// module exists to remove.
    pub fn usb_has(&self, application: &str) -> Option<bool> {
        if self.usb_enabled.is_empty() && self.usb_supported.is_empty() {
            return None;
        }
        Some(
            self.usb_enabled
                .iter()
                .any(|a| a.eq_ignore_ascii_case(application)),
        )
    }

    /// One line per fact, for the diagnostics report and the applet panel. Never a
    /// secret: this applet holds none.
    pub fn describe(&self) -> Vec<String> {
        let mut lines = Vec::new();
        if let Some(form_factor) = &self.form_factor {
            lines.push(format!("form factor: {form_factor}"));
        }
        if !self.usb_enabled.is_empty() || !self.usb_supported.is_empty() {
            lines.push(format!(
                "USB applications enabled: [{}] of [{}]",
                self.usb_enabled.join(", "),
                self.usb_supported.join(", ")
            ));
        }
        if self.has_nfc() {
            lines.push(format!(
                "NFC applications enabled: [{}] of [{}]",
                self.nfc_enabled.join(", "),
                self.nfc_supported.join(", ")
            ));
        }
        if self.fips_capable_device || !self.fips_capable.is_empty() {
            lines.push(format!(
                "FIPS: device model {}, capable [{}], approved [{}]",
                if self.fips_capable_device {
                    "yes"
                } else {
                    "no"
                },
                self.fips_capable.join(", "),
                self.fips_approved.join(", ")
            ));
        }
        if self.config_locked {
            lines.push(
                "the device configuration is locked with a code — which applications are enabled \
                 cannot be changed without it"
                    .to_owned(),
            );
        }
        lines
    }
}

/// Parse one `READ CONFIG` page: a length byte, then the TLVs.
///
/// The length byte is checked rather than skipped. A response whose declared
/// length disagrees with what arrived is a truncated read, and the fields most
/// likely to be missing from the tail are the capability masks — silently
/// producing "nothing enabled".
pub fn parse_page(response: &[u8]) -> Option<Vec<(u32, Vec<u8>)>> {
    let (declared, body) = response.split_first()?;
    if usize::from(*declared) != body.len() {
        return None;
    }
    Some(
        tlvs(body)
            .into_iter()
            .map(|(tag, value)| (tag, value.to_vec()))
            .collect(),
    )
}

/// Turn the collected TLVs of every page into the answer.
pub fn from_tlvs(fields: &[(u32, Vec<u8>)]) -> DeviceConfig {
    let value = |tag: u32| -> Option<&[u8]> {
        fields
            .iter()
            .find(|(t, _)| *t == tag)
            .map(|(_, v)| v.as_slice())
    };
    let mask = |tag: u32| -> Vec<String> {
        value(tag)
            .and_then(be_integer)
            .map(names)
            .unwrap_or_default()
    };

    let form_factor_byte = value(TAG_FORM_FACTOR).and_then(be_integer).unwrap_or(0);

    DeviceConfig {
        // A serial of zero is how the applet says "this key does not report one",
        // and `Option` is how that is carried onward rather than as key 0.
        serial: value(TAG_SERIAL)
            .and_then(be_integer)
            .filter(|serial| *serial > 0)
            .and_then(|serial| u32::try_from(serial).ok()),
        firmware: value(TAG_VERSION).and_then(version),
        form_factor: form_factor(form_factor_byte),
        usb_enabled: mask(TAG_USB_ENABLED),
        usb_supported: mask(TAG_USB_SUPPORTED),
        nfc_enabled: mask(TAG_NFC_ENABLED),
        nfc_supported: mask(TAG_NFC_SUPPORTED),
        fips_capable_device: form_factor_byte & 0x80 != 0,
        fips_capable: value(TAG_FIPS_CAPABLE)
            .and_then(be_integer)
            .map(fips_names)
            .unwrap_or_default(),
        fips_approved: value(TAG_FIPS_APPROVED)
            .and_then(be_integer)
            .map(fips_names)
            .unwrap_or_default(),
        security_key: form_factor_byte & 0x40 != 0,
        config_locked: value(TAG_CONFIG_LOCK) == Some(&[0x01]),
    }
}

/// How many further pages the applet says are waiting.
pub fn more_pages(fields: &[(u32, Vec<u8>)]) -> u8 {
    fields
        .iter()
        .find(|(tag, _)| *tag == TAG_MORE_DATA)
        .and_then(|(_, value)| be_integer(value))
        .and_then(|n| u8::try_from(n).ok())
        .unwrap_or(0)
}

/// The application names a capability mask names, in the order the bits are
/// defined so two reads of the same key never differ by ordering.
fn names(mask: u64) -> Vec<String> {
    CAPABILITIES
        .iter()
        .filter(|(bit, _)| mask & bit != 0)
        .map(|(_, name)| (*name).to_owned())
        .collect()
}

/// The FIPS masks are a **different** encoding from the capability masks: one bit
/// per application in the order FIDO2, PIV, OpenPGP, OATH, YubiHSM Auth, not the
/// capability bit values. Reading them with [`names`] reports OTP and U2F instead,
/// which is wrong in the direction of overclaiming compliance.
fn fips_names(mask: u64) -> Vec<String> {
    const FIPS_BITS: [(u64, &str); 5] = [
        (1 << 0, "FIDO2"),
        (1 << 1, "PIV"),
        (1 << 2, "OpenPGP"),
        (1 << 3, "OATH"),
        (1 << 4, "YubiHSM Auth"),
    ];
    FIPS_BITS
        .iter()
        .filter(|(bit, _)| mask & bit != 0)
        .map(|(_, name)| (*name).to_owned())
        .collect()
}

/// Three bytes, major.minor.patch.
fn version(value: &[u8]) -> Option<String> {
    match value {
        [major, minor, patch] => Some(format!("{major}.{minor}.{patch}")),
        _ => None,
    }
}

/// The form factor, in the wording `ykman` prints — so the register does not hold
/// two spellings of the same physical key depending on which transport read it.
fn form_factor(byte: u64) -> Option<String> {
    Some(
        match byte & 0x0F {
            0x01 => "Keychain (USB-A)",
            0x02 => "Nano (USB-A)",
            0x03 => "Keychain (USB-C)",
            0x04 => "Nano (USB-C)",
            0x05 => "Keychain (USB-C, Lightning)",
            0x06 => "Bio (USB-A)",
            0x07 => "Bio (USB-C)",
            // Zero is "the applet did not say", and every other value is a form
            // factor this build has never heard of. Both are reported as unknown
            // rather than guessed, and zero as nothing at all.
            0x00 => return None,
            _ => "Unknown",
        }
        .to_owned(),
    )
}

/// Read the management applet of the key with this serial.
///
/// Read-only, so it is safe from a screen the operator merely opened — the rule
/// `AGENTS.md` states for hardware and the reason [`super::applets`] exists.
#[cfg(feature = "native-piv")]
pub fn read(serial: u32) -> super::write::Result<DeviceConfig> {
    const OP: &str = "mgmt.read_config";
    let mut session = Session::open(serial, OP)?;
    let mut fields: Vec<(u32, Vec<u8>)> = Vec::new();
    let mut page = 0u8;

    loop {
        let response = session.read_config(page, OP)?;
        let parsed = parse_page(&response).ok_or(super::write::WriteError::Failed {
            operation: OP,
            reason: "the management applet's answer was truncated — which applications are \
                     enabled cannot be read from a partial response"
                .into(),
        })?;
        let more = more_pages(&parsed);
        fields.extend(parsed);
        page += 1;
        if more == 0 || page >= MAX_PAGES {
            break;
        }
    }

    Ok(from_tlvs(&fields))
}

/// Not compiled without a card transport. The caller reports the gap; returning a
/// default here would say "nothing is enabled".
#[cfg(not(feature = "native-piv"))]
pub fn read(serial: u32) -> super::write::Result<DeviceConfig> {
    let _ = serial;
    Err(super::write::WriteError::TransportUnavailable {
        operation: "mgmt.read_config",
        feature: "native-piv",
    })
}

/// The CCID conversation with the management applet.
///
/// Its own session rather than a method on [`super::piv_session::Session`]: that
/// one has selected the PIV applet, and selecting another on the same card
/// discards PIV's authentication state. Two short sessions cost a reconnect; one
/// shared session costs an authentication nobody expected to lose.
#[cfg(feature = "native-piv")]
struct Session {
    card: pcsc::Card,
}

#[cfg(feature = "native-piv")]
impl Session {
    fn open(serial: u32, operation: &'static str) -> super::write::Result<Self> {
        use super::write::WriteError;

        let ctx = pcsc::Context::establish(pcsc::Scope::User).map_err(|e| WriteError::Failed {
            operation,
            reason: format!("no PC/SC service: {e}"),
        })?;
        let mut names = vec![0u8; ctx.list_readers_len().unwrap_or(2048)];
        let readers = ctx
            .list_readers(&mut names)
            .map_err(|e| WriteError::Failed {
                operation,
                reason: format!("no readers: {e}"),
            })?;

        for reader in readers {
            if !reader.to_string_lossy().to_lowercase().contains("yubikey") {
                continue;
            }
            let Ok(card) = ctx.connect(reader, pcsc::ShareMode::Shared, pcsc::Protocols::ANY)
            else {
                continue;
            };
            let mut candidate = Self { card };
            if candidate.select(operation).is_err() {
                continue;
            }
            // The applet reports the serial itself, which is what identifies the
            // key — a reader name carries the model and nothing more.
            let Ok(response) = candidate.read_config(0, operation) else {
                continue;
            };
            let matches = parse_page(&response)
                .map(|fields| from_tlvs(&fields).serial == Some(serial))
                .unwrap_or(false);
            if matches {
                return Ok(candidate);
            }
        }
        Err(WriteError::NotAttached(serial))
    }

    fn select(&mut self, operation: &'static str) -> super::write::Result<()> {
        let mut apdu = vec![0x00, 0xA4, 0x04, 0x00, MGMT_AID.len() as u8];
        apdu.extend_from_slice(&MGMT_AID);
        let (_, sw) = self.transmit(&apdu, operation)?;
        if sw == 0x9000 {
            Ok(())
        } else {
            Err(super::write::WriteError::Unsupported {
                operation,
                reason: format!(
                    "the management application did not answer over CCID (status 0x{sw:04x}) — a \
                     key with CCID disabled, or firmware below 5.0"
                ),
            })
        }
    }

    fn read_config(&mut self, page: u8, operation: &'static str) -> super::write::Result<Vec<u8>> {
        let (data, sw) = self.transmit(&[0x00, INS_READ_CONFIG, page, 0x00, 0x00], operation)?;
        if sw != 0x9000 {
            return Err(super::write::WriteError::Failed {
                operation,
                reason: format!("reading the device configuration: card status 0x{sw:04x}"),
            });
        }
        Ok(data)
    }

    /// Send one APDU, following `61xx` continuations.
    fn transmit(
        &mut self,
        apdu: &[u8],
        operation: &'static str,
    ) -> super::write::Result<(Vec<u8>, u16)> {
        let mut collected = Vec::new();
        let mut request = apdu.to_vec();
        loop {
            let mut buf = vec![0u8; 1024];
            let response = self.card.transmit(&request, &mut buf).map_err(|e| {
                super::write::WriteError::Failed {
                    operation,
                    reason: format!("the card did not answer: {e}"),
                }
            })?;
            if response.len() < 2 {
                return Err(super::write::WriteError::Failed {
                    operation,
                    reason: "truncated response from the card".into(),
                });
            }
            let split = response.len() - 2;
            let sw = u16::from(response[split]) << 8 | u16::from(response[split + 1]);
            collected.extend_from_slice(&response[..split]);
            if sw & 0xFF00 == 0x6100 {
                request = vec![0x00, 0xC0, 0x00, 0x00, (sw & 0x00FF) as u8];
                continue;
            }
            return Ok((collected, sw));
        }
    }
}

/// Fold a management read into a [`super::DeviceInfo`] the rest of the app already
/// understands.
///
/// The PIV applet answers for serial and firmware and this applet answers for
/// everything else, so this takes the identification a transport already has and
/// fills the gaps rather than replacing it. A field the applet did not report is
/// left exactly as it was — which is the whole discipline of this module.
pub fn enrich(info: &mut super::DeviceInfo, config: &DeviceConfig) {
    if let Some(form_factor) = &config.form_factor
        && info.form_factor.is_empty()
    {
        info.form_factor = form_factor.clone();
    }
    if let Some(firmware) = &config.firmware
        && info.firmware.is_empty()
    {
        info.firmware = firmware.clone();
    }
    if !config.usb_enabled.is_empty() {
        info.usb_applications = config.usb_enabled.clone();
    }
    // NFC is a fact about the hardware, and the applet is the only thing that
    // reports it natively. `false` stays `false` when nothing was read.
    if config.has_nfc() {
        info.nfc = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::tlv::push_len;

    /// Build a `READ CONFIG` response the way the applet frames one: a length
    /// byte, then the TLVs.
    fn page(fields: &[(u32, &[u8])]) -> Vec<u8> {
        let mut body = Vec::new();
        for (tag, value) in fields {
            assert!(*tag <= 0xFF, "the management applet uses one-byte tags");
            body.push(*tag as u8);
            push_len(&mut body, value.len());
            body.extend_from_slice(value);
        }
        let mut out = vec![body.len() as u8];
        out.extend(body);
        out
    }

    /// A YubiKey 5 NFC with everything enabled — the key the recorded `ykman info`
    /// fixture came from, encoded as the applet would report it.
    fn five_nfc() -> Vec<u8> {
        page(&[
            (TAG_USB_SUPPORTED, &[0x03, 0x3B]),
            (TAG_SERIAL, &[0x01, 0x37, 0xA3, 0xD1]),
            (TAG_USB_ENABLED, &[0x03, 0x3B]),
            (TAG_FORM_FACTOR, &[0x01]),
            (TAG_VERSION, &[5, 4, 3]),
            (TAG_NFC_SUPPORTED, &[0x03, 0x3B]),
            (TAG_NFC_ENABLED, &[0x03, 0x3B]),
        ])
    }

    #[test]
    fn a_fully_enabled_key_reports_every_application_by_the_name_ykman_uses() {
        // The names are load-bearing: `bootstrap::preflight` matches on them, so a
        // native read that spelled an application differently from the `ykman` read
        // would skip a step on one transport and run it on the other.
        let fields = parse_page(&five_nfc()).expect("the fixture is well formed");
        let config = from_tlvs(&fields);

        assert_eq!(config.serial, Some(20_423_633));
        assert_eq!(config.firmware.as_deref(), Some("5.4.3"));
        assert_eq!(config.form_factor.as_deref(), Some("Keychain (USB-A)"));
        assert_eq!(
            config.usb_enabled,
            vec![
                "Yubico OTP",
                "FIDO U2F",
                "OpenPGP",
                "PIV",
                "OATH",
                "YubiHSM Auth",
                "FIDO2"
            ]
        );
        assert!(config.has_nfc());
        assert!(!config.config_locked);
        assert!(!config.fips_capable_device);
    }

    #[test]
    fn the_names_are_the_ones_the_pre_flight_matches_against() {
        // Written as the assertion the pre-flight actually makes, so a rename here
        // fails a test rather than quietly skipping a step.
        let config = from_tlvs(&parse_page(&five_nfc()).unwrap());
        for applet in ["FIDO2", "PIV", "OTP"] {
            assert!(
                config
                    .usb_enabled
                    .iter()
                    .any(|a| a.to_uppercase().contains(applet)),
                "the pre-flight looks for `{applet}` in {:?}",
                config.usb_enabled
            );
        }
    }

    #[test]
    fn a_key_with_an_application_switched_off_says_so_rather_than_omitting_it() {
        // The read this whole module is for: OTP disabled is a *fact*, and it is
        // what turns a step into a skip instead of a warning.
        let response = page(&[
            (TAG_USB_SUPPORTED, &[0x03, 0x3F]),
            (TAG_USB_ENABLED, &[0x03, 0x3E]),
            (TAG_SERIAL, &[0x01, 0x37, 0xC1, 0xD1]),
        ]);
        let config = from_tlvs(&parse_page(&response).unwrap());

        assert_eq!(config.usb_has("Yubico OTP"), Some(false));
        assert_eq!(config.usb_has("PIV"), Some(true));
        assert!(
            config.usb_supported.iter().any(|a| a == "Yubico OTP"),
            "the hardware still has it — it is turned off, not absent: {:?}",
            config.usb_supported
        );
    }

    #[test]
    fn an_applet_that_said_nothing_about_applications_is_unknown_and_not_disabled() {
        // The distinction the two historical bugs turned on. `None` must never be
        // read as "disabled".
        let config = DeviceConfig::default();
        assert_eq!(config.usb_has("PIV"), None);
        assert_eq!(config.usb_has("FIDO2"), None);
        assert!(!config.has_nfc());
        assert!(config.describe().is_empty());
    }

    #[test]
    fn a_fips_key_is_read_as_a_model_and_its_approved_applications_separately() {
        // Being able to be FIPS-approved and being in that state are different
        // claims, and the second is the one a compliance statement rests on.
        let response = page(&[
            (TAG_SERIAL, &[0x01, 0xDF, 0x5E, 0x76]),
            (TAG_FORM_FACTOR, &[0x83]),
            (TAG_VERSION, &[5, 7, 1]),
            (TAG_FIPS_CAPABLE, &[0x1F]),
            (TAG_FIPS_APPROVED, &[0x02]),
        ]);
        let config = from_tlvs(&parse_page(&response).unwrap());

        assert!(config.fips_capable_device);
        assert_eq!(config.form_factor.as_deref(), Some("Keychain (USB-C)"));
        assert!(config.fips_capable.iter().any(|a| a == "PIV"));
        assert_eq!(
            config.fips_approved,
            vec!["PIV"],
            "capable is not approved: {:?}",
            config.fips_approved
        );
        assert!(
            config.describe().iter().any(|l| l.contains("FIPS")),
            "{:?}",
            config.describe()
        );
    }

    #[test]
    fn the_fips_masks_are_not_read_with_the_capability_bits() {
        // The bug this test exists for: the two masks look alike and are not. Bit 1
        // is PIV in the FIPS encoding and FIDO U2F in the capability encoding, so
        // reading one with the other's table overclaims compliance.
        assert_eq!(fips_names(1 << 1), vec!["PIV"]);
        assert_eq!(names(1 << 1), vec!["FIDO U2F"]);
        assert_eq!(fips_names(0), Vec::<String>::new());
    }

    #[test]
    fn a_security_key_is_recognised_from_the_form_factor_byte() {
        let response = page(&[(TAG_SERIAL, &[0x01]), (TAG_FORM_FACTOR, &[0x43])]);
        let config = from_tlvs(&parse_page(&response).unwrap());
        assert!(config.security_key);
        assert_eq!(config.form_factor.as_deref(), Some("Keychain (USB-C)"));
    }

    #[test]
    fn a_locked_configuration_is_reported_because_it_explains_a_missing_application() {
        let response = page(&[
            (TAG_SERIAL, &[0x01]),
            (TAG_USB_ENABLED, &[0x00, 0x10]),
            (TAG_CONFIG_LOCK, &[0x01]),
        ]);
        let config = from_tlvs(&parse_page(&response).unwrap());
        assert!(config.config_locked);
        assert!(
            config.describe().iter().any(|l| l.contains("locked")),
            "{:?}",
            config.describe()
        );
    }

    #[test]
    fn a_truncated_response_is_refused_rather_than_read_as_nothing_enabled() {
        // The failure this length check is for. Dropping the tail of the response
        // drops the capability masks, and a missing mask reads as "no applications"
        // — which would skip every step of the procedure.
        let mut response = five_nfc();
        response.truncate(response.len() - 4);
        assert!(
            parse_page(&response).is_none(),
            "a response whose declared length disagrees with its body is not a read"
        );
        assert!(parse_page(&[]).is_none());
        assert!(parse_page(&[0x05]).is_none());
    }

    #[test]
    fn a_second_page_is_asked_for_only_when_the_applet_says_there_is_one() {
        let with_more = parse_page(&page(&[(TAG_MORE_DATA, &[0x01]), (TAG_SERIAL, &[0x01])]))
            .expect("well formed");
        assert_eq!(more_pages(&with_more), 1);
        assert_eq!(more_pages(&parse_page(&five_nfc()).unwrap()), 0);
    }

    #[test]
    fn an_unknown_form_factor_is_unknown_and_an_absent_one_is_absent() {
        assert_eq!(form_factor(0x00), None, "nothing said is not a form factor");
        assert_eq!(form_factor(0x0F).as_deref(), Some("Unknown"));
        assert_eq!(form_factor(0x86).as_deref(), Some("Bio (USB-A)"));
    }

    #[test]
    fn a_serial_of_zero_is_reported_as_no_serial() {
        let config = from_tlvs(&parse_page(&page(&[(TAG_SERIAL, &[0x00])])).unwrap());
        assert_eq!(config.serial, None, "key 0 is not a key");
    }

    #[test]
    fn enriching_fills_the_gaps_and_overwrites_nothing_that_was_read() {
        // The rule: the PIV applet answers for serial and firmware, this one for the
        // rest, and neither is allowed to erase the other's answer.
        let config = from_tlvs(&parse_page(&five_nfc()).unwrap());

        let mut native = super::super::DeviceInfo {
            serial: 20_423_633,
            model: "YubiKey CCID".into(),
            firmware: "5.4.3".into(),
            ..Default::default()
        };
        enrich(&mut native, &config);
        assert_eq!(native.firmware, "5.4.3");
        assert_eq!(native.form_factor, "Keychain (USB-A)");
        assert!(native.nfc);
        assert!(native.usb_applications.iter().any(|a| a == "FIDO2"));

        // Nothing read means nothing changed — including the empty application
        // list, which stays empty rather than becoming a claim.
        let mut untouched = super::super::DeviceInfo {
            serial: 1,
            firmware: "5.7.4".into(),
            form_factor: "Nano (USB-C)".into(),
            ..Default::default()
        };
        enrich(&mut untouched, &DeviceConfig::default());
        assert_eq!(untouched.firmware, "5.7.4");
        assert_eq!(untouched.form_factor, "Nano (USB-C)");
        assert!(untouched.usb_applications.is_empty());
        assert!(!untouched.nfc);
    }

    #[test]
    fn a_build_with_no_card_transport_says_so_instead_of_answering() {
        // Only meaningful in the `ykman`-only build; asserted there so the honest
        // failure is covered rather than assumed.
        #[cfg(not(feature = "native-piv"))]
        {
            let err = read(20_423_633).unwrap_err();
            assert!(err.detail().contains("native-piv"), "{}", err.detail());
        }
    }
}
