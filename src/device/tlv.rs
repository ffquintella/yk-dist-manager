//! BER-TLV, the encoding every applet on a YubiKey answers in.
//!
//! Extracted from [`super::piv_session`] when the **management** applet needed the
//! same walker (`features/native-device-transport.md` phase 5). Two copies of a
//! length parser is how one of them ends up subtly different from the other, and
//! the failure mode is not a compile error — it is a field read out of the middle
//! of the wrong bytes.
//!
//! Always compiled, unlike either caller: nothing here needs a card, so it stays
//! inside the coverage gate and is exercised by every build rather than only by
//! the one with `native-piv` on.

/// Walk a BER-TLV sequence one level deep, handling multi-byte tags and long
/// lengths.
///
/// Both forms are needed and neither is exotic: a PIV generate response is tagged
/// `7F 49` (two bytes) and a certificate object carries its length in two. A
/// parser that assumed one byte of each read a 16-byte witness correctly and then
/// silently mis-read everything larger.
///
/// A malformed sequence stops the walk and yields what was read up to that point,
/// rather than erroring or panicking: these bytes come off a card, and a caller's
/// question is always "is the tag I need in here", which an empty answer answers.
pub fn tlvs(data: &[u8]) -> Vec<(u32, &[u8])> {
    let mut out = Vec::new();
    let mut i = 0;

    while i < data.len() {
        let mut tag = u32::from(data[i]);
        // A tag whose low five bits are all set continues into the next bytes,
        // each with the high bit meaning "one more".
        if data[i] & 0x1F == 0x1F {
            loop {
                i += 1;
                if i >= data.len() {
                    return out;
                }
                tag = (tag << 8) | u32::from(data[i]);
                if data[i] & 0x80 == 0 {
                    break;
                }
            }
        }
        i += 1;
        if i >= data.len() {
            return out;
        }

        let first = data[i];
        i += 1;
        let len = if first < 0x80 {
            first as usize
        } else {
            let count = (first & 0x7F) as usize;
            if count == 0 || count > 4 || i + count > data.len() {
                return out;
            }
            let mut len = 0usize;
            for _ in 0..count {
                len = (len << 8) | data[i] as usize;
                i += 1;
            }
            len
        };

        if i + len > data.len() {
            return out;
        }
        out.push((tag, &data[i..i + len]));
        i += len;
    }
    out
}

/// The value of one tag at the top level of a sequence.
pub fn find_tlv(data: &[u8], tag: u32) -> Option<&[u8]> {
    tlvs(data)
        .into_iter()
        .find(|(t, _)| *t == tag)
        .map(|(_, v)| v)
}

/// The value of a tag nested one level inside another.
pub fn inner_tlv(data: &[u8], outer: u32, inner: u32) -> Option<&[u8]> {
    find_tlv(data, outer).and_then(|value| find_tlv(value, inner))
}

/// A big-endian integer held in a TLV value, as the management applet encodes
/// capability masks and serial numbers.
///
/// Anything wider than eight bytes is refused rather than truncated: a truncated
/// capability mask would read as *fewer applications enabled*, which downstream is
/// a claim that a step should be skipped.
pub fn be_integer(value: &[u8]) -> Option<u64> {
    if value.is_empty() || value.len() > 8 {
        return None;
    }
    Some(value.iter().fold(0u64, |acc, b| (acc << 8) | u64::from(*b)))
}

/// Append a BER length: short form, or `81`/`82` with the count.
pub fn push_len(out: &mut Vec<u8>, len: usize) {
    if len < 0x80 {
        out.push(len as u8);
    } else if len <= 0xFF {
        out.push(0x81);
        out.push(len as u8);
    } else {
        out.push(0x82);
        out.push((len >> 8) as u8);
        out.push(len as u8);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tlv_parsing_finds_a_nested_tag() {
        // The PIV authentication witness as the card actually frames it: 0x80
        // inside 0x7C.
        let response = [
            0x7C, 0x12, 0x80, 0x10, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16,
        ];
        let witness = inner_tlv(&response, 0x7C, 0x80).expect("the witness is there");
        assert_eq!(witness.len(), 16);
        assert_eq!(witness[0], 1);
        assert_eq!(inner_tlv(&response, 0x7C, 0x82), None);
    }

    #[test]
    fn a_truncated_tlv_does_not_panic() {
        assert!(tlvs(&[0x7C, 0x10, 0x01]).is_empty());
        assert!(inner_tlv(&[0x7C], 0x7C, 0x80).is_none());
        assert!(tlvs(&[0x7F]).is_empty());
        assert!(tlvs(&[0x7F, 0x49]).is_empty());
        assert!(tlvs(&[0x53, 0x82, 0x01]).is_empty());
    }

    #[test]
    fn a_two_byte_tag_is_read_as_one_tag() {
        // `7F 49` is how every PIV generate response is framed. A parser that read
        // one-byte tags saw `7F` with a nonsense length and returned nothing.
        let response = [0x7F, 0x49, 0x04, 0x86, 0x02, 0xAA, 0xBB];
        assert_eq!(
            find_tlv(&response, 0x7F49).map(|v| v.to_vec()),
            Some(vec![0x86, 0x02, 0xAA, 0xBB])
        );
        assert_eq!(
            inner_tlv(&response, 0x7F49, 0x86).map(|v| v.to_vec()),
            Some(vec![0xAA, 0xBB])
        );
    }

    #[test]
    fn a_long_length_is_read_as_a_length() {
        // A certificate object is past 255 bytes, so `82 xx xx` is the normal case
        // rather than the exotic one.
        let mut data = vec![0x53, 0x82, 0x01, 0x00];
        data.extend(std::iter::repeat_n(0xEE, 256));
        assert_eq!(find_tlv(&data, 0x53).map(|v| v.len()), Some(256));

        let mut short = vec![0x70, 0x81, 0x80];
        short.extend(std::iter::repeat_n(0x11, 128));
        assert_eq!(find_tlv(&short, 0x70).map(|v| v.len()), Some(128));
    }

    #[test]
    fn lengths_round_trip_through_the_parser() {
        for len in [0usize, 1, 127, 128, 255, 256, 4096] {
            let mut encoded = vec![0x70];
            push_len(&mut encoded, len);
            encoded.extend(std::iter::repeat_n(0x5A, len));
            assert_eq!(
                find_tlv(&encoded, 0x70).map(|v| v.len()),
                Some(len),
                "length {len}"
            );
        }
    }

    #[test]
    fn an_integer_value_is_read_big_endian_and_never_truncated() {
        assert_eq!(be_integer(&[0x02, 0x3F]), Some(0x023F));
        assert_eq!(be_integer(&[0x01]), Some(1));
        assert_eq!(be_integer(&[]), None, "an absent value is not zero");
        assert_eq!(
            be_integer(&[0u8; 9]),
            None,
            "a mask wider than the reader is refused rather than cut down — a cut-down \
             capability mask reads as applications being disabled"
        );
    }
}
