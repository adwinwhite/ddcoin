use std::fmt::{Display, Formatter};

use ethnum::U256;
use serde::{Deserialize, Serialize};

use crate::Config;

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

// a 256 bit target level.
#[derive(Eq, PartialEq, Clone, Copy, Hash, Debug, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Difficulty(U256);

impl Difficulty {
    pub const fn new(d: U256) -> Self {
        Self(d)
    }
    pub fn adjust_with_actual_span(&self, actual: Timestamp, config: &Config) -> Self {
        let expected: U256 = (config.average_block_time().as_nanos()
            * u128::from(config.difficulty_adjustment_period()))
        .into();
        let actual: U256 = actual.into();
        if actual > expected * U256::from(4_u32) {
            Self(self.0.saturating_mul(U256::from(4_u32)))
        } else if actual < expected / U256::from(4_u32) {
            Self((self.0 / U256::from(4_u32)).max(U256::ONE))
        } else {
            Self((self.0 / expected).max(U256::ONE).saturating_mul(actual))
        }
    }

    pub fn is_met(&self, hash: [u8; 32]) -> bool {
        let hash = U256::from_le_bytes(hash);
        hash < self.0
    }
}

impl From<U256> for Difficulty {
    fn from(value: U256) -> Self {
        Self(value)
    }
}
