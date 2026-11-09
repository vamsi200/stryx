use crate::error::{DecodeError, StryxError};

pub(crate) struct Decoder<'a> {
    data: &'a [u8],
    pub pos: usize,
}

impl<'a> Decoder<'a> {
    pub(crate) fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    pub(crate) fn read_bytes(&mut self, len: usize) -> Result<&'a [u8], StryxError> {
        let end = self
            .pos
            .checked_add(len)
            .ok_or(DecodeError::InvalidLength)?;

        let bytes = self.data.get(self.pos..end).ok_or(DecodeError::Truncated)?;

        self.pos = end;

        Ok(bytes)
    }

    pub(crate) fn read_u8(&mut self) -> Result<u8, StryxError> {
        let bytes = self.read_bytes(1)?;
        Ok(u8::from_le_bytes(
            bytes.try_into().map_err(DecodeError::from)?,
        ))
    }

    pub(crate) fn read_u16(&mut self) -> Result<u16, StryxError> {
        let bytes = self.read_bytes(2)?;
        Ok(u16::from_le_bytes(
            bytes.try_into().map_err(DecodeError::from)?,
        ))
    }

    pub(crate) fn read_u32(&mut self) -> Result<u32, StryxError> {
        let bytes = self.read_bytes(4)?;
        Ok(u32::from_le_bytes(
            bytes.try_into().map_err(DecodeError::from)?,
        ))
    }

    pub(crate) fn read_u64(&mut self) -> Result<u64, StryxError> {
        let bytes = self.read_bytes(8)?;
        Ok(u64::from_le_bytes(
            bytes.try_into().map_err(DecodeError::from)?,
        ))
    }

    pub(crate) fn read_i32(&mut self) -> Result<i32, StryxError> {
        let bytes = self.read_bytes(4)?;
        Ok(i32::from_le_bytes(
            bytes.try_into().map_err(DecodeError::from)?,
        ))
    }

    #[allow(dead_code)]
    pub(crate) fn position(&self) -> usize {
        self.pos
    }

    pub(crate) fn remaining(&self) -> usize {
        self.data.len() - self.pos
    }
}
