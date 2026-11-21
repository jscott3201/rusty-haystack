//! Bounded two-entry ZIP profile. No filesystem operations or ignored entries.
use super::PackageError as E;
use flate2::{Decompress, FlushDecompress, Status};

pub(super) const MAX_ARCHIVE: usize = 1024 * 1024;
pub(super) const MAX_SOURCE: usize = 65_536;
pub(super) const MAX_META: usize = 16_384;
fn bytes(input: &[u8], at: usize, len: usize) -> Result<&[u8], E> {
    input
        .get(at..at.checked_add(len).ok_or(E::Invalid("ZIP extent"))?)
        .ok_or(E::Invalid("truncated ZIP"))
}
fn u16_at(input: &[u8], at: usize) -> Result<u16, E> {
    Ok(u16::from_le_bytes(bytes(input, at, 2)?.try_into().unwrap()))
}
fn u32_at(input: &[u8], at: usize) -> Result<u32, E> {
    Ok(u32::from_le_bytes(bytes(input, at, 4)?.try_into().unwrap()))
}
fn signature(input: &[u8], at: usize, value: u32) -> Result<(), E> {
    if u32_at(input, at)? != value {
        return Err(E::Invalid("ZIP signature"));
    }
    Ok(())
}
/// Exact complete archive, including unsigned/signed data descriptors.
pub(super) fn unpack(input: &[u8]) -> Result<(String, String), E> {
    if input.len() > MAX_ARCHIVE {
        return Err(E::Limit("archive bytes"));
    }
    let end = input.len().checked_sub(22).ok_or(E::Invalid("ZIP end"))?;
    signature(input, end, 0x06054b50)?;
    if u16_at(input, end + 4)? != 0 || u16_at(input, end + 6)? != 0 {
        return Err(E::Unsupported("multidisk ZIP"));
    }
    if u16_at(input, end + 8)? != 2 || u16_at(input, end + 10)? != 2 {
        return Err(E::Unsupported("exactly two ZIP entries required"));
    }
    if u16_at(input, end + 20)? != 0 {
        return Err(E::Unsupported("ZIP comment"));
    }
    let central_len = u32_at(input, end + 12)? as usize;
    let central_start = u32_at(input, end + 16)? as usize;
    if central_start.checked_add(central_len) != Some(end) {
        return Err(E::Invalid("ZIP directory extent"));
    }
    let mut central = central_start;
    let mut local = 0usize;
    let mut result = Vec::with_capacity(2);
    for (name, limit) in [("xeto-meta.props", MAX_META), ("lib.xeto", MAX_SOURCE)] {
        signature(input, central, 0x02014b50)?;
        let flags = u16_at(input, central + 8)?;
        let method = u16_at(input, central + 10)?;
        if !matches!(method, 0 | 8) {
            return Err(E::Unsupported("ZIP compression"));
        }
        let allowed = if method == 8 { 0x080e } else { 0x0808 };
        if flags & !allowed != 0 || u16_at(input, central + 6)? > 20 {
            return Err(E::Unsupported("ZIP flags or version"));
        }
        let crc = u32_at(input, central + 16)?;
        let compressed = u32_at(input, central + 20)? as usize;
        let expanded = u32_at(input, central + 24)? as usize;
        if expanded > limit {
            return Err(E::Limit("expanded entry bytes"));
        }
        let name_len = u16_at(input, central + 28)? as usize;
        if bytes(input, central + 46, name_len)? != name.as_bytes() {
            return Err(E::Unsupported("ZIP entry name or ordering"));
        }
        if u16_at(input, central + 30)? != 0
            || u16_at(input, central + 32)? != 0
            || u16_at(input, central + 34)? != 0
        {
            return Err(E::Unsupported("ZIP extra fields, comments or disk"));
        }
        let attr = u32_at(input, central + 38)?;
        let file_type = (attr >> 16) & 0xf000;
        if !matches!(file_type, 0 | 0x8000) || attr & 0x18 != 0 {
            return Err(E::Unsupported("ZIP nonregular entry"));
        }
        if u32_at(input, central + 42)? as usize != local {
            return Err(E::Invalid("ZIP local entry offset"));
        }
        signature(input, local, 0x04034b50)?;
        if u16_at(input, local + 4)? != u16_at(input, central + 6)?
            || u16_at(input, local + 6)? != flags
            || u16_at(input, local + 8)? != method
            || bytes(input, local + 10, 4)? != bytes(input, central + 12, 4)?
            || u16_at(input, local + 26)? as usize != name_len
            || u16_at(input, local + 28)? != 0
            || bytes(input, local + 30, name_len)? != name.as_bytes()
        {
            return Err(E::Invalid("ZIP local and central headers differ"));
        }
        let local_values = [
            u32_at(input, local + 14)?,
            u32_at(input, local + 18)?,
            u32_at(input, local + 22)?,
        ];
        let expected = [crc, compressed as u32, expanded as u32];
        if (flags & 8 == 0 && local_values != expected)
            || (flags & 8 != 0 && local_values != [0; 3] && local_values != expected)
        {
            return Err(E::Invalid("ZIP local sizes or CRC"));
        }
        let data_at = local + 30 + name_len;
        let data = bytes(input, data_at, compressed)?;
        local = data_at
            .checked_add(compressed)
            .ok_or(E::Invalid("ZIP data extent"))?;
        if flags & 8 != 0 {
            let unsigned = [
                u32_at(input, local)?,
                u32_at(input, local + 4)?,
                u32_at(input, local + 8)?,
            ];
            if unsigned == expected {
                local += 12;
            } else {
                signature(input, local, 0x08074b50)?;
                if [
                    u32_at(input, local + 4)?,
                    u32_at(input, local + 8)?,
                    u32_at(input, local + 12)?,
                ] != expected
                {
                    return Err(E::Invalid("ZIP data descriptor"));
                }
                local += 16;
            }
        }
        if local > central_start {
            return Err(E::Invalid("ZIP entry overlaps directory"));
        }
        let output = if method == 0 {
            if compressed != expanded {
                return Err(E::Invalid("stored ZIP length"));
            }
            data.to_vec()
        } else {
            // One bounded buffer; declared size never permits unbounded expansion.
            let mut output = vec![0; expanded + 1];
            let mut decoder = Decompress::new(false);
            let status = decoder
                .decompress(data, &mut output, FlushDecompress::Finish)
                .map_err(|_| E::Invalid("DEFLATE stream"))?;
            if status != Status::StreamEnd
                || decoder.total_in() != compressed as u64
                || decoder.total_out() != expanded as u64
            {
                return Err(E::Invalid("DEFLATE size or trailing input"));
            }
            output.truncate(expanded);
            output
        };
        if crc32fast::hash(&output) != crc {
            return Err(E::Invalid("ZIP CRC"));
        }
        result.push(String::from_utf8(output).map_err(|_| E::Invalid("package UTF-8"))?);
        central += 46 + name_len;
    }
    if local != central_start || central != end {
        return Err(E::Invalid("unclaimed ZIP bytes"));
    }
    Ok((result.remove(0), result.remove(0)))
}
