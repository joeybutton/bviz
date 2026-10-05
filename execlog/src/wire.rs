//! Protobuf's wire format, as far as reading chosen fields of a message without
//! decoding it whole: https://protobuf.dev/programming-guides/encoding/

use prost::DecodeError;

/// The longest encoding of a varint, a 64-bit one.
pub(crate) const MAX_VARINT_BYTES: usize = 10;

/// Set on every byte of a varint but its last.
pub(crate) const VARINT_CONTINUES: u8 = 0x80;

/// The value bits each varint byte carries.
const VARINT_BITS: u32 = 7;

const VARINT: u64 = 0;
const FIXED64: u64 = 1;
const LENGTH_DELIMITED: u64 = 2;
const FIXED32: u64 = 5;

/// A field key's low bits, which hold the wire type; the rest hold the field
/// number.
const WIRE_TYPE_BITS: u32 = 3;
const WIRE_TYPE_MASK: u64 = (1 << WIRE_TYPE_BITS) - 1;

/// A field's value, as far as the wire format tells it.
pub(crate) enum Value<'a> {
    Varint(u64),
    /// A string, bytes, an embedded message, or a packed repeated field.
    Bytes(&'a [u8]),
    Fixed,
}

/// The value of an int32 field read as `varint`: a negative one is encoded
/// sign-extended to 64 bits, so its low 32 bits are the value.
pub(crate) fn int32(varint: u64) -> i32 {
    varint as i32
}

/// The field number and value at the head of `message`, which it advances
/// past the field.
pub(crate) fn next_field<'a>(message: &mut &'a [u8]) -> Result<(u32, Value<'a>), DecodeError> {
    let key = varint(message)?;
    let number =
        u32::try_from(key >> WIRE_TYPE_BITS).map_err(|_| DecodeError::new("invalid key"))?;
    let value = match key & WIRE_TYPE_MASK {
        VARINT => Value::Varint(varint(message)?),
        LENGTH_DELIMITED => {
            let length = usize::try_from(varint(message)?).unwrap_or(usize::MAX);
            Value::Bytes(take(message, length)?)
        }
        FIXED64 => {
            take(message, size_of::<u64>())?;
            Value::Fixed
        }
        FIXED32 => {
            take(message, size_of::<u32>())?;
            Value::Fixed
        }
        _ => return Err(DecodeError::new("unsupported wire type")),
    };
    Ok((number, value))
}

fn take<'a>(message: &mut &'a [u8], length: usize) -> Result<&'a [u8], DecodeError> {
    if length > message.len() {
        return Err(DecodeError::new("buffer underflow"));
    }
    let (head, rest) = message.split_at(length);
    *message = rest;
    Ok(head)
}

/// The varint at the head of `bytes`, which it advances past the varint.
pub(crate) fn varint(bytes: &mut &[u8]) -> Result<u64, DecodeError> {
    let mut value = 0;
    for (i, &byte) in bytes.iter().take(MAX_VARINT_BYTES).enumerate() {
        value |= u64::from(byte & !VARINT_CONTINUES) << (i as u32 * VARINT_BITS);
        if byte & VARINT_CONTINUES == 0 {
            *bytes = &bytes[i + 1..];
            return Ok(value);
        }
    }
    Err(DecodeError::new("invalid varint"))
}

pub(crate) fn text(bytes: &[u8]) -> Result<&str, DecodeError> {
    std::str::from_utf8(bytes).map_err(|_| DecodeError::new("string is not UTF-8"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn varint_reads_what_prost_writes() {
        for value in [0, 1, 127, 128, 300, u32::MAX as usize] {
            let mut encoded = Vec::new();
            prost::encode_length_delimiter(value, &mut encoded).unwrap();
            encoded.push(0xff);
            let mut bytes = encoded.as_slice();
            assert_eq!(varint(&mut bytes).unwrap(), value as u64);
            assert_eq!(bytes, [0xff]);
        }
        let mut widest: &[u8] = &[0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x01];
        assert_eq!(varint(&mut widest).unwrap(), u64::MAX);
        assert!(varint(&mut [0x80, 0x80].as_slice()).is_err());
    }

    #[test]
    fn next_field_reads_each_wire_type() {
        let mut message: &[u8] = &[
            0x08, 0x96, 0x01, // field 1, varint 150
            0x12, 0x02, b'h', b'i', // field 2, bytes "hi"
            0x19, 0, 0, 0, 0, 0, 0, 0, 0, // field 3, fixed64
            0x25, 0, 0, 0, 0, // field 4, fixed32
        ];
        assert!(matches!(
            next_field(&mut message),
            Ok((1, Value::Varint(150)))
        ));
        assert!(matches!(
            next_field(&mut message),
            Ok((2, Value::Bytes(b"hi")))
        ));
        assert!(matches!(next_field(&mut message), Ok((3, Value::Fixed))));
        assert!(matches!(next_field(&mut message), Ok((4, Value::Fixed))));
        assert!(message.is_empty());
        assert!(next_field(&mut [0x12, 0x05, b'h'].as_slice()).is_err());
    }
}
