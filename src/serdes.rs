pub mod transport {
    use anyhow::Result;
    use serde::{Deserialize, Serialize};

    const BINCODE_CONFIG: bincode::config::Configuration = bincode::config::standard();

    pub fn encode<T: Serialize>(msg: &T) -> Result<Vec<u8>> {
        let encoded = bincode::serde::encode_to_vec(msg, BINCODE_CONFIG)?;
        Ok(encoded)
    }

    pub fn decode<T: for<'de> Deserialize<'de>>(buf: &[u8]) -> Result<T> {
        let decoded = bincode::serde::decode_from_slice(buf, BINCODE_CONFIG)?.0;
        Ok(decoded)
    }
}

pub mod hashsig {
    use anyhow::Result;
    use serde::Serialize;

    const BINCODE_CONFIG: bincode::config::Configuration<
        bincode::config::LittleEndian,
        bincode::config::Fixint,
    > = bincode::config::standard().with_fixed_int_encoding();

    pub fn encode<T: Serialize>(msg: &T) -> Result<Vec<u8>> {
        let encoded = bincode::serde::encode_to_vec(msg, BINCODE_CONFIG)?;
        Ok(encoded)
    }
}
