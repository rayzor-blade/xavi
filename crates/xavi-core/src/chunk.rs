use std::sync::Arc;

use crate::{Error, Result, snapshot};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EncodedChunkType {
    Key,
    Delta,
}

/// Encoded bytes are immutable; cloning retains them without another byte copy.
/// Duration None (unknown) is distinct from Some(0). Negative PTS is preserved.
#[derive(Clone, Debug)]
pub struct EncodedChunk {
    kind: EncodedChunkType,
    timestamp: i64,
    duration: Option<u64>,
    data: Arc<[u8]>,
}

impl EncodedChunk {
    pub fn new(
        kind: EncodedChunkType,
        timestamp: i64,
        duration: Option<u64>,
        data: &[u8],
    ) -> Result<Self> {
        Ok(Self {
            kind,
            timestamp,
            duration,
            data: snapshot(data)?,
        })
    }

    pub fn kind(&self) -> EncodedChunkType {
        self.kind
    }
    pub fn timestamp(&self) -> i64 {
        self.timestamp
    }
    pub fn duration(&self) -> Option<u64> {
        self.duration
    }
    pub fn byte_len(&self) -> u32 {
        self.data.len() as u32
    }
    pub fn bytes(&self) -> &[u8] {
        &self.data
    }

    pub fn copy_to(&self, destination: &mut [u8]) -> Result<()> {
        if destination.len() < self.data.len() {
            return Err(Error::invalid("encoded chunk destination is too small"));
        }
        destination[..self.data.len()].copy_from_slice(&self.data);
        Ok(())
    }
}
