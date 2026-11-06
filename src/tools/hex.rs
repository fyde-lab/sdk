//! Lowercase hex encoding, for digests and other bytes shown as text.

/// `bytes` as lowercase hex, two digits per byte.
pub(crate) fn encode(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";

    let mut hex = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        hex.push(char::from(DIGITS[usize::from(byte >> 4)]));
        hex.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    hex
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_writes_two_lowercase_digits_per_byte() {
        assert_eq!(
            encode(&[0x00, 0x01, 0x0f, 0x10, 0xab, 0xff]),
            "00010f10abff"
        );
    }

    #[test]
    fn encode_of_nothing_is_empty() {
        assert_eq!(encode(&[]), "");
    }
}
