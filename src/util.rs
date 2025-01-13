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
