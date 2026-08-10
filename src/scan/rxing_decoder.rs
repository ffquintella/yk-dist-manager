//! Barcode decoding with [`rxing`](https://crates.io/crates/rxing), the Rust port
//! of ZXing. Pure Rust, no system library, so it works the same on every platform.
//!
//! Behind the `barcode` feature.

use super::{BarcodeDecoder, LumaFrame, ScanError};

/// Decodes 1D and 2D barcodes from a luminance frame.
///
/// Yubico packaging uses linear barcodes (Code 128 / Code 39 family) for the
/// serial, and some bulk labels carry a QR or Data Matrix instead. `rxing`'s
/// multi-format reader covers all of them, so the decoder does not need to know
/// which the label used.
#[derive(Debug, Default, Clone, Copy)]
pub struct RxingDecoder {
    /// Try harder: slower, and markedly better on a hand-held camera frame where
    /// the label is at an angle. Worth it — a scan that fails silently costs the
    /// operator more than 50ms.
    pub thorough: bool,
}

impl RxingDecoder {
    pub fn new() -> Self {
        Self { thorough: true }
    }

    /// Decode a barcode from an image file — a photo of the label taken with a
    /// phone, or a saved frame. Useful without a camera, and the path used by the
    /// decoder's own tests.
    pub fn decode_image_file(&self, path: &std::path::Path) -> Result<Vec<String>, ScanError> {
        let image = image::open(path).map_err(|e| ScanError::Decoder(e.to_string()))?;
        let luma = image.to_luma8();
        let frame = LumaFrame::new(luma.width(), luma.height(), luma.into_raw())
            .ok_or_else(|| ScanError::Decoder("image has no pixels".into()))?;
        self.decode(&frame)
    }
}

impl BarcodeDecoder for RxingDecoder {
    fn decode(&self, frame: &LumaFrame) -> Result<Vec<String>, ScanError> {
        // Try every barcode in the frame first: a box label often carries two,
        // and reading both lets `serial_from_texts` cross-check them.
        let multiple =
            rxing::helpers::detect_multiple_in_luma(frame.data.clone(), frame.width, frame.height);

        if let Ok(results) = multiple
            && !results.is_empty()
        {
            return Ok(results
                .into_iter()
                .map(|result| result.getText().to_owned())
                .collect());
        }

        // The multi-reader is stricter about partially visible codes than the
        // single-code reader, so fall back rather than reporting nothing.
        match rxing::helpers::detect_in_luma(frame.data.clone(), frame.width, frame.height, None) {
            Ok(result) => Ok(vec![result.getText().to_owned()]),
            Err(_) => Err(ScanError::NoBarcode),
        }
    }

    fn describe(&self) -> String {
        format!(
            "rxing (multi-format{})",
            if self.thorough { ", thorough" } else { "" }
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::scan_frame;

    /// Render a Code 128 barcode for `text` as a luminance frame, using rxing's
    /// own encoder — so the test exercises the real decoder rather than a stub.
    fn barcode_frame(text: &str) -> LumaFrame {
        use rxing::Writer;

        let writer = rxing::oned::Code128Writer;
        let matrix = writer
            .encode(text, &rxing::BarcodeFormat::CODE_128, 400, 120)
            .expect("encodes");

        let width = matrix.getWidth();
        let height = matrix.getHeight();
        let mut data = Vec::with_capacity((width * height) as usize);
        for y in 0..height {
            for x in 0..width {
                // A set bit is a bar (black).
                data.push(if matrix.get(x, y) { 0 } else { 255 });
            }
        }
        LumaFrame::new(width, height, data).expect("frame")
    }

    #[test]
    fn decodes_a_rendered_code128_serial() {
        let frame = barcode_frame("20423633");
        let decoder = RxingDecoder::new();

        let texts = decoder.decode(&frame).expect("decodes");
        assert!(
            texts.iter().any(|t| t.contains("20423633")),
            "decoded {texts:?}"
        );
        assert_eq!(scan_frame(&decoder, &frame).unwrap(), 20_423_633);
    }

    #[test]
    fn decodes_a_label_with_a_prefix() {
        let frame = barcode_frame("SN20423633");
        let decoder = RxingDecoder::new();
        assert_eq!(scan_frame(&decoder, &frame).unwrap(), 20_423_633);
    }

    #[test]
    fn a_blank_frame_is_no_barcode_not_a_panic() {
        let frame = LumaFrame::new(64, 64, vec![255; 64 * 64]).unwrap();
        let decoder = RxingDecoder::new();
        assert_eq!(decoder.decode(&frame).unwrap_err(), ScanError::NoBarcode);
    }

    #[test]
    fn a_barcode_that_is_not_a_serial_is_reported_as_such() {
        let frame = barcode_frame("HELLO");
        let decoder = RxingDecoder::new();
        // It decodes fine; it just is not a serial.
        assert!(decoder.decode(&frame).is_ok());
        assert!(scan_frame(&decoder, &frame).is_err());
    }

    #[test]
    fn the_decoder_describes_itself_for_the_settings_screen() {
        assert!(RxingDecoder::new().describe().contains("rxing"));
    }
}
