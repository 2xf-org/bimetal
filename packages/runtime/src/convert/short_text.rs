//! Short text on the stack, which a conversion writes where it would
//! otherwise allocate a string, such as for Serde's `collect_str`.

use std::fmt;

/// Short text on the stack, such as the RFC 3339 text of a date-time.
pub(crate) struct ShortText {
    bytes: [u8; Self::CAPACITY],
    length: usize,
}

impl ShortText {
    /// Room for any date-time, `YYYY-MM-DDTHH:MM:SS.fffffffff+HH:MM:SS`.
    const CAPACITY: usize = 64;

    pub(crate) fn new() -> Self {
        Self {
            bytes: [0; Self::CAPACITY],
            length: 0,
        }
    }

    pub(crate) fn as_str(&self) -> &str {
        std::str::from_utf8(&self.bytes[..self.length])
            .expect("the text holds the whole characters that it was given")
    }

    /// Append `byte`, an ASCII character.
    #[cfg(feature = "python")]
    pub(crate) fn push(&mut self, byte: u8) -> Result<(), Full> {
        *self.bytes.get_mut(self.length).ok_or(Full)? = byte;
        self.length += 1;
        Ok(())
    }

    /// Append `value` in `width` digits, with leading zeros.
    #[cfg(feature = "python")]
    pub(crate) fn digits(&mut self, mut value: u32, width: usize) -> Result<(), Full> {
        let end = self.length + width;
        let slot = self.bytes.get_mut(self.length..end).ok_or(Full)?;
        for byte in slot.iter_mut().rev() {
            *byte = b"0123456789"[(value % 10) as usize];
            value /= 10;
        }
        if value != 0 {
            return Err(Full);
        }
        self.length = end;
        Ok(())
    }
}

impl fmt::Write for ShortText {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        let end = self.length + text.len();
        let slot = self.bytes.get_mut(self.length..end).ok_or(fmt::Error)?;
        slot.copy_from_slice(text.as_bytes());
        self.length = end;
        Ok(())
    }

    // chrono writes a date-time one character at a time.
    fn write_char(&mut self, character: char) -> fmt::Result {
        match u8::try_from(character) {
            Ok(byte) if byte.is_ascii() => {
                *self.bytes.get_mut(self.length).ok_or(fmt::Error)? = byte;
                self.length += 1;
                Ok(())
            }
            _ => self.write_str(character.encode_utf8(&mut [0; 4])),
        }
    }
}

/// A value that does not fit its room in a [`ShortText`].
#[cfg(feature = "python")]
#[derive(Debug)]
pub(crate) struct Full;
