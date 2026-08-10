//! Reading a serial number from a **barcode** — the label on the box, or a
//! camera pointed at it.
//!
//! Why this exists: receiving a shipment means getting fifty serials into the
//! inventory. Plugging each key in is the accurate way and the slow way; the
//! packaging carries the serial as a barcode, so scanning the labels records the
//! shipment in minutes and each key gets *verified* later, when it is bootstrapped
//! ([`crate::domain::SerialSource`] keeps the two apart).
//!
//! Layering, so the useful part needs no camera and no hardware:
//!
//! | Layer | Feature | Testable headless |
//! |---|---|---|
//! | [`parse_serial`] — pull a serial out of decoded text | always on | yes |
//! | [`BarcodeDecoder`] — decode a luminance frame | always on (trait) | yes, with a stub |
//! | [`rxing_decoder`] — the real decoder | `barcode` | yes, against a rendered fixture |
//! | [`camera`] — live frames | `camera` | no (needs a camera) |
//!
//! A cheaper alternative that needs none of this: a **USB barcode scanner**
//! emulates a keyboard, so it types the serial into the focused field. The
//! inventory's serial box accepts that today, and the wedge is what a busy
//! receiving desk usually wants. The camera path is for the operator who has a
//! laptop and no scanner.

#[cfg(feature = "camera")]
pub mod camera;
#[cfg(feature = "barcode")]
pub mod rxing_decoder;

#[cfg(feature = "barcode")]
pub use rxing_decoder::RxingDecoder;

/// Yubico serials are 7–8 digits today; accept a slightly wider band so a future
/// batch does not silently fail to scan, and reject anything implausible.
pub const MIN_SERIAL: u32 = 100_000; // 6 digits
pub const MAX_SERIAL: u32 = 999_999_999; // 9 digits

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ScanError {
    #[error("no barcode found in the image")]
    NoBarcode,
    #[error("barcode decoded as `{0}`, which contains no serial number")]
    NoSerial(String),
    #[error("`{found}` is not a plausible YubiKey serial number")]
    Implausible { found: String },
    #[error(
        "two different serials in the same image ({first} and {second}) — scan one label at a time"
    )]
    Ambiguous { first: u32, second: u32 },
    #[error("camera error: {0}")]
    Camera(String),
    #[error("decoder error: {0}")]
    Decoder(String),
}

/// A grayscale frame: one byte per pixel, row-major, `width * height` long.
///
/// Deliberately not an `image` type, so the trait and its tests do not depend on
/// the `barcode` feature.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LumaFrame {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

impl LumaFrame {
    pub fn new(width: u32, height: u32, data: Vec<u8>) -> Option<Self> {
        if width == 0 || height == 0 || data.len() != (width as usize * height as usize) {
            return None;
        }
        Some(Self {
            width,
            height,
            data,
        })
    }

    /// Build a frame from packed RGB (3 bytes per pixel), as a camera delivers it.
    ///
    /// Rec. 601 luma, which is what barcode decoders expect.
    pub fn from_rgb(width: u32, height: u32, rgb: &[u8]) -> Option<Self> {
        let pixels = width as usize * height as usize;
        if pixels == 0 || rgb.len() < pixels * 3 {
            return None;
        }
        let mut data = Vec::with_capacity(pixels);
        for pixel in rgb.chunks_exact(3).take(pixels) {
            let luma = 0.299 * f32::from(pixel[0])
                + 0.587 * f32::from(pixel[1])
                + 0.114 * f32::from(pixel[2]);
            data.push(luma.round().clamp(0.0, 255.0) as u8);
        }
        Self::new(width, height, data)
    }
}

/// Anything that can find barcodes in a frame and return their text.
///
/// Implementations return **every** barcode they find: deciding what to do with
/// two labels in shot is [`serial_from_texts`]'s job, not the decoder's.
pub trait BarcodeDecoder {
    fn decode(&self, frame: &LumaFrame) -> Result<Vec<String>, ScanError>;

    /// Short description for the settings screen.
    fn describe(&self) -> String;
}

/// Extract a serial from one decoded barcode payload.
///
/// Labels are not uniform: some encode the bare serial (`20423633`), some prefix
/// it (`S/N: 20423633`), some carry a URL or a key-value blob. So the rule is:
/// find digit runs, keep the plausible ones, and refuse when a payload offers two
/// different candidates rather than guessing which is the serial.
pub fn parse_serial(text: &str) -> Result<u32, ScanError> {
    let mut candidates: Vec<u32> = Vec::new();

    for run in text.split(|c: char| !c.is_ascii_digit()) {
        if run.is_empty() {
            continue;
        }
        // A run longer than the widest plausible serial is something else
        // entirely (a URL id, a timestamp) — do not truncate it into a serial.
        if run.len() > 9 {
            continue;
        }
        if let Ok(value) = run.parse::<u32>()
            && (MIN_SERIAL..=MAX_SERIAL).contains(&value)
            && !candidates.contains(&value)
        {
            candidates.push(value);
        }
    }

    match candidates.as_slice() {
        [] => {
            // Distinguish "no digits at all" from "digits, but not a serial", so
            // the operator knows whether they scanned the wrong label.
            if text.chars().any(|c| c.is_ascii_digit()) {
                Err(ScanError::Implausible {
                    found: text.trim().to_owned(),
                })
            } else {
                Err(ScanError::NoSerial(text.trim().to_owned()))
            }
        }
        [only] => Ok(*only),
        [first, second, ..] => Err(ScanError::Ambiguous {
            first: *first,
            second: *second,
        }),
    }
}

/// Reduce every barcode found in one frame to a single serial.
///
/// Two labels showing the same serial is fine (a box often carries the barcode
/// twice); two *different* serials is refused, because picking one would attribute
/// a credential to whichever key happened to be closer to the lens.
pub fn serial_from_texts(texts: &[String]) -> Result<u32, ScanError> {
    if texts.is_empty() {
        return Err(ScanError::NoBarcode);
    }

    let mut found: Option<u32> = None;
    let mut last_error: Option<ScanError> = None;

    for text in texts {
        match parse_serial(text) {
            Ok(serial) => match found {
                None => found = Some(serial),
                Some(existing) if existing == serial => {}
                Some(existing) => {
                    return Err(ScanError::Ambiguous {
                        first: existing,
                        second: serial,
                    });
                }
            },
            Err(e) => last_error = Some(e),
        }
    }

    found.ok_or_else(|| last_error.unwrap_or(ScanError::NoBarcode))
}

/// Decode a frame and reduce it to one serial.
pub fn scan_frame(decoder: &dyn BarcodeDecoder, frame: &LumaFrame) -> Result<u32, ScanError> {
    serial_from_texts(&decoder.decode(frame)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bare_serial_parses() {
        assert_eq!(parse_serial("20423633").unwrap(), 20_423_633);
    }

    #[test]
    fn a_prefixed_or_decorated_serial_parses() {
        assert_eq!(parse_serial("S/N: 20423633").unwrap(), 20_423_633);
        assert_eq!(parse_serial("  20423633\n").unwrap(), 20_423_633);
        assert_eq!(parse_serial("YubiKey 5 NFC #20423633").unwrap(), 20_423_633);
    }

    #[test]
    fn a_repeated_serial_in_one_payload_is_not_ambiguous() {
        assert_eq!(parse_serial("20423633 20423633").unwrap(), 20_423_633);
    }

    #[test]
    fn two_different_serials_are_refused_rather_than_guessed() {
        assert_eq!(
            parse_serial("20423633 / 31415926").unwrap_err(),
            ScanError::Ambiguous {
                first: 20_423_633,
                second: 31_415_926
            }
        );
    }

    #[test]
    fn text_without_digits_says_so() {
        assert!(matches!(
            parse_serial("YubiKey NFC").unwrap_err(),
            ScanError::NoSerial(_)
        ));
    }

    #[test]
    fn digits_that_are_not_a_serial_are_a_different_complaint() {
        // "YubiKey 5 NFC" has a digit, so the operator scanned a real label —
        // just not one carrying a serial. Saying "no serial" instead of "no
        // digits" is what tells them which.
        assert!(matches!(
            parse_serial("YubiKey 5 NFC").unwrap_err(),
            ScanError::Implausible { .. }
        ));
    }

    #[test]
    fn an_implausible_number_is_rejected() {
        // Too short to be a serial, and a long id is not truncated into one.
        assert!(matches!(
            parse_serial("42").unwrap_err(),
            ScanError::Implausible { .. }
        ));
        assert!(matches!(
            parse_serial("1234567890123").unwrap_err(),
            ScanError::Implausible { .. }
        ));
    }

    #[test]
    fn several_barcodes_agreeing_yield_one_serial() {
        let texts = vec!["20423633".to_owned(), "S/N 20423633".to_owned()];
        assert_eq!(serial_from_texts(&texts).unwrap(), 20_423_633);
    }

    #[test]
    fn several_barcodes_disagreeing_are_refused() {
        let texts = vec!["20423633".to_owned(), "31415926".to_owned()];
        assert!(matches!(
            serial_from_texts(&texts).unwrap_err(),
            ScanError::Ambiguous { .. }
        ));
    }

    #[test]
    fn an_empty_decode_is_no_barcode() {
        assert_eq!(serial_from_texts(&[]).unwrap_err(), ScanError::NoBarcode);
    }

    #[test]
    fn an_unusable_payload_reports_why_not_just_no_barcode() {
        let texts = vec!["https://yubico.com".to_owned()];
        assert!(matches!(
            serial_from_texts(&texts).unwrap_err(),
            ScanError::Implausible { .. } | ScanError::NoSerial(_)
        ));
    }

    #[test]
    fn frames_validate_their_dimensions() {
        assert!(LumaFrame::new(2, 2, vec![0; 4]).is_some());
        assert!(LumaFrame::new(2, 2, vec![0; 3]).is_none());
        assert!(LumaFrame::new(0, 2, vec![]).is_none());
    }

    #[test]
    fn rgb_converts_to_luma() {
        // White, black, red.
        let rgb = vec![255, 255, 255, 0, 0, 0, 255, 0, 0];
        let frame = LumaFrame::from_rgb(3, 1, &rgb).expect("converts");
        assert_eq!(frame.data[0], 255);
        assert_eq!(frame.data[1], 0);
        assert_eq!(frame.data[2], 76, "Rec. 601 luma of pure red");
    }

    #[test]
    fn a_short_rgb_buffer_is_refused() {
        assert!(LumaFrame::from_rgb(4, 4, &[0; 10]).is_none());
    }

    /// A decoder that returns whatever it was told to, so the reduction logic can
    /// be tested without an image or the `barcode` feature.
    struct StubDecoder(Vec<String>);

    impl BarcodeDecoder for StubDecoder {
        fn decode(&self, _frame: &LumaFrame) -> Result<Vec<String>, ScanError> {
            Ok(self.0.clone())
        }

        fn describe(&self) -> String {
            "stub".into()
        }
    }

    #[test]
    fn scanning_a_frame_goes_from_barcode_text_to_serial() {
        let frame = LumaFrame::new(2, 2, vec![0; 4]).unwrap();
        let decoder = StubDecoder(vec!["S/N: 20423633".to_owned()]);
        assert_eq!(scan_frame(&decoder, &frame).unwrap(), 20_423_633);

        let empty = StubDecoder(vec![]);
        assert_eq!(
            scan_frame(&empty, &frame).unwrap_err(),
            ScanError::NoBarcode
        );
    }
}
