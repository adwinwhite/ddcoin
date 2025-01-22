use std::fmt::{Display, Formatter};

use serde::{Deserialize, Serialize};

pub(crate) const fn hex_to_bytes<const N: usize>(s: &str) -> [u8; N] {
    const fn ascii_to_num(c: u8) -> u8 {
        match c {
            b'0'..=b'9' => c - b'0',
            b'a'..=b'f' => c - b'a' + 10,
            b'A'..=b'F' => c - b'A' + 10,
            _ => panic!("non-hex character"),
        }
    }
    if !s.is_ascii() {
        panic!("contains non-ascii characters");
    }
    if s.len() != N * 2 {
        panic!("invalid length");
    }

    let s = s.as_bytes();
    let mut bytes = [0; N];
    let mut i = 0;
    loop {
        if i >= N {
            break;
        }
        let high = ascii_to_num(s[i * 2]);
        let low = ascii_to_num(s[i * 2 + 1]);
        let byte = (high << 4) | low;
        bytes[i] = byte;
        i += 1;
    }
    bytes
}

// Use big endian as that's what std function do.
pub fn num_of_zeros_in_sha256(bytes: &[u8; 32]) -> u32 {
    let first_half = bytes[..16].try_into().unwrap();
    let first = u128::from_be_bytes(first_half);
    let first_num = first.leading_zeros();
    if first_num < 128 {
        first_num
    } else {
        let second_half = bytes[16..32].try_into().unwrap();
        let second = u128::from_be_bytes(second_half);
        let second_num = second.leading_zeros();
        first_num + second_num
    }
}

pub(crate) fn fmt_hex(bytes: &[u8], f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    for b in bytes {
        write!(f, "{:02x}", b)?;
    }
    Ok(())
}

// nanoseconds.
// TODO: consider a more approriate type. u128 doesn't port well to other languages?
pub trait TimestampExt {
    fn now() -> Self;
}
pub type Timestamp = u128;
impl TimestampExt for Timestamp {
    fn now() -> Timestamp {
        std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap(/* Cannot be eariler than epoch */)
        .as_nanos()
    }
}

#[derive(Eq, PartialEq, Clone, Copy, Hash, Debug, Serialize, Deserialize)]
pub struct Sha256Hash(pub [u8; 32]);

impl Display for Sha256Hash {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        fmt_hex(&self.0, f)
    }
}

impl From<[u8; 32]> for Sha256Hash {
    fn from(value: [u8; 32]) -> Self {
        Self(value)
    }
}
