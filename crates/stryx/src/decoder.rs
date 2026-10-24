use crate::error::{DecodeError, StryxError};

pub struct Decoder<'a> {
    data: &'a [u8],
    pub pos: usize,
}

impl<'a> Decoder<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    pub fn read_bytes(&mut self, len: usize) -> Result<&'a [u8], StryxError> {
        let end = self
            .pos
            .checked_add(len)
            .ok_or(DecodeError::InvalidLength)?;

        let bytes = self.data.get(self.pos..end).ok_or(DecodeError::Truncated)?;

        self.pos = end;

        Ok(bytes)
    }

    pub fn read_u16(&mut self) -> Result<u16, StryxError> {
        let bytes = self.read_bytes(2)?;
        Ok(u16::from_le_bytes(
            bytes.try_into().map_err(DecodeError::from)?,
        ))
    }

    pub fn read_u32(&mut self) -> Result<u32, StryxError> {
        let bytes = self.read_bytes(4)?;
        Ok(u32::from_le_bytes(
            bytes.try_into().map_err(DecodeError::from)?,
        ))
    }

    pub fn read_u64(&mut self) -> Result<u64, StryxError> {
        let bytes = self.read_bytes(8)?;
        Ok(u64::from_le_bytes(
            bytes.try_into().map_err(DecodeError::from)?,
        ))
    }

    pub fn position(&self) -> usize {
        self.pos
    }

    pub fn remaining(&self) -> usize {
        self.data.len() - self.pos
    }
}
