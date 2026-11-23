use std::io::{self, ErrorKind};

fn varint(mut value: u64, out: &mut Vec<u8>) {
    while value >= 128 {
        out.push(value as u8 | 128);
        value >>= 7;
    }
    out.push(value as u8);
}

pub fn number(field: u64, value: u64, out: &mut Vec<u8>) {
    varint(field << 3, out);
    varint(value, out);
}

pub fn bytes(field: u64, value: impl AsRef<[u8]>, out: &mut Vec<u8>) {
    let value = value.as_ref();
    varint((field << 3) | 2, out);
    varint(value.len() as u64, out);
    out.extend_from_slice(value);
}

fn invalid() -> io::Error {
    io::Error::new(ErrorKind::InvalidData, "invalid Antigravity protobuf")
}

fn read_varint(input: &mut &[u8]) -> io::Result<u64> {
    let mut result = 0;
    for shift in (0..64).step_by(7) {
        let (&byte, rest) = input.split_first().ok_or_else(invalid)?;
        *input = rest;
        if shift == 63 && byte > 1 {
            return Err(invalid());
        }
        result |= u64::from(byte & 127) << shift;
        if byte < 128 {
            return Ok(result);
        }
    }
    Err(invalid())
}

pub fn get(mut input: &[u8], wanted: u64) -> io::Result<Option<&[u8]>> {
    while !input.is_empty() {
        let tag = read_varint(&mut input)?;
        let length = match tag & 7 {
            0 => {
                read_varint(&mut input)?;
                continue;
            }
            1 => 8,
            2 => usize::try_from(read_varint(&mut input)?).map_err(|_| invalid())?,
            5 => 4,
            _ => return Err(invalid()),
        };
        let (value, rest) = input.split_at_checked(length).ok_or_else(invalid)?;
        if tag & 7 == 2 && tag >> 3 == wanted {
            return Ok(Some(value));
        }
        input = rest;
    }
    Ok(None)
}

pub fn integer(mut input: &[u8], wanted: u64) -> io::Result<u64> {
    while !input.is_empty() {
        let tag = read_varint(&mut input)?;
        let length = match tag & 7 {
            0 => {
                let value = read_varint(&mut input)?;
                if tag >> 3 == wanted {
                    return Ok(value);
                }
                continue;
            }
            1 => 8,
            2 => usize::try_from(read_varint(&mut input)?).map_err(|_| invalid())?,
            5 => 4,
            _ => return Err(invalid()),
        };
        input = input.get(length..).ok_or_else(invalid)?;
    }
    Ok(0)
}

// Preserve fields we don't know about when repairing a summary written by agy.
pub fn replace_bytes(input: &[u8], fields: &[(u64, &[u8])]) -> io::Result<Vec<u8>> {
    let mut remaining = input;
    let mut output = Vec::with_capacity(input.len());
    while !remaining.is_empty() {
        let start = remaining;
        let tag = read_varint(&mut remaining)?;
        let length = match tag & 7 {
            0 => {
                read_varint(&mut remaining)?;
                0
            }
            1 => 8,
            2 => usize::try_from(read_varint(&mut remaining)?).map_err(|_| invalid())?,
            5 => 4,
            _ => return Err(invalid()),
        };
        remaining = remaining.get(length..).ok_or_else(invalid)?;
        if !fields.iter().any(|(field, _)| *field == tag >> 3) {
            output.extend_from_slice(&start[..start.len() - remaining.len()]);
        }
    }
    for &(field, value) in fields {
        bytes(field, value, &mut output);
    }
    Ok(output)
}
