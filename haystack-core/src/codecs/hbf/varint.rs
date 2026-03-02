//! LEB128 unsigned varint encoding and decoding.

use super::error::HbfError;

/// Encode a u64 value as an unsigned LEB128 varint, appending bytes to `buf`.
pub fn encode_varint(buf: &mut Vec<u8>, mut val: u64) {
    loop {
        let byte = (val & 0x7F) as u8;
        val >>= 7;
        if val == 0 {
            buf.push(byte);
            return;
        }
        buf.push(byte | 0x80);
    }
}

/// Decode an unsigned LEB128 varint from `data` starting at `*pos`.
/// Advances `*pos` past the consumed bytes on success.
pub fn decode_varint(data: &[u8], pos: &mut usize) -> Result<u64, HbfError> {
    let mut result: u64 = 0;
    let mut shift = 0u32;
    loop {
        if *pos >= data.len() {
            return Err(HbfError::Eof);
        }
        let byte = data[*pos];
        *pos += 1;
        result |= ((byte & 0x7F) as u64) << shift;
        if byte & 0x80 == 0 {
            return Ok(result);
        }
        shift += 7;
        if shift >= 64 {
            return Err(HbfError::Message("varint too long".into()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn varint_roundtrip_zero() {
        let mut buf = Vec::new();
        encode_varint(&mut buf, 0);
        let mut pos = 0;
        assert_eq!(decode_varint(&buf, &mut pos).unwrap(), 0);
        assert_eq!(pos, 1);
    }

    #[test]
    fn varint_roundtrip_small() {
        let mut buf = Vec::new();
        encode_varint(&mut buf, 127);
        assert_eq!(buf.len(), 1);
        let mut pos = 0;
        assert_eq!(decode_varint(&buf, &mut pos).unwrap(), 127);
    }

    #[test]
    fn varint_roundtrip_multibyte() {
        let mut buf = Vec::new();
        encode_varint(&mut buf, 300);
        assert!(buf.len() > 1);
        let mut pos = 0;
        assert_eq!(decode_varint(&buf, &mut pos).unwrap(), 300);
    }

    #[test]
    fn varint_roundtrip_large() {
        let mut buf = Vec::new();
        encode_varint(&mut buf, u64::MAX);
        let mut pos = 0;
        assert_eq!(decode_varint(&buf, &mut pos).unwrap(), u64::MAX);
    }

    #[test]
    fn varint_eof() {
        let buf = [];
        let mut pos = 0;
        assert!(decode_varint(&buf, &mut pos).is_err());
    }
}
