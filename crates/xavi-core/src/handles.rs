//! Positive 32-bit resource handles, local to one backend instance.
//!
//! Bits 26..30 carry kind, 16..25 generation, and 0..15 slot index. Zero is
//! invalid. A slot is permanently retired before its generation could wrap,
//! so no stale handle can ever alias a later resource in the same table.

use std::sync::Arc;

use crate::{Error, Result};

const INDEX_BITS: u32 = 16;
const GENERATION_BITS: u32 = 10;
const INDEX_MASK: u32 = (1 << INDEX_BITS) - 1;
const GENERATION_MAX: u16 = (1 << GENERATION_BITS) - 1;
const KIND_SHIFT: u32 = INDEX_BITS + GENERATION_BITS;
const MAX_SLOTS: usize = 1 << INDEX_BITS;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum Kind {
    AudioData = 1,
    VideoFrame = 2,
    EncodedAudioChunk = 3,
    EncodedVideoChunk = 4,
    PlaneLayouts = 5,
    MediaPlayer = 6,
    CodecConfiguration = 7,
    MediaQueue = 8,
    MediaCodec = 9,
    MediaMuxer = 10,
    MediaDemuxer = 11,
}

impl Kind {
    pub fn of(handle: i32) -> Option<Self> {
        if handle <= 0 {
            return None;
        }
        match (handle as u32) >> KIND_SHIFT {
            1 => Some(Self::AudioData),
            2 => Some(Self::VideoFrame),
            3 => Some(Self::EncodedAudioChunk),
            4 => Some(Self::EncodedVideoChunk),
            5 => Some(Self::PlaneLayouts),
            6 => Some(Self::MediaPlayer),
            7 => Some(Self::CodecConfiguration),
            8 => Some(Self::MediaQueue),
            9 => Some(Self::MediaCodec),
            10 => Some(Self::MediaMuxer),
            11 => Some(Self::MediaDemuxer),
            _ => None,
        }
    }
}

struct Slot<T> {
    generation: u16,
    value: Option<Arc<T>>,
    next_free: Option<usize>,
}

pub struct Slab<T> {
    kind: Kind,
    slots: Vec<Slot<T>>,
    free: Option<usize>,
    live: usize,
}

impl<T> Slab<T> {
    pub const fn new(kind: Kind) -> Self {
        Self {
            kind,
            slots: Vec::new(),
            free: None,
            live: 0,
        }
    }

    pub fn insert(&mut self, value: T) -> Result<i32> {
        self.insert_shared(Arc::new(value))
    }

    /// Makes a distinct handle to an already owned resource, without copying it.
    pub fn insert_shared(&mut self, value: Arc<T>) -> Result<i32> {
        let index = match self.free {
            Some(index) => {
                self.free = self.slots[index].next_free.take();
                index
            }
            None => {
                if self.slots.len() == MAX_SLOTS {
                    return Err(Error::exhausted());
                }
                self.slots.try_reserve(1).map_err(|_| Error::exhausted())?;
                let index = self.slots.len();
                self.slots.push(Slot {
                    generation: 0,
                    value: None,
                    next_free: None,
                });
                index
            }
        };
        let slot = &mut self.slots[index];
        slot.value = Some(value);
        self.live += 1;
        Ok(((self.kind as u32) << KIND_SHIFT
            | u32::from(slot.generation) << INDEX_BITS
            | index as u32) as i32)
    }

    /// Retains the resource independently of the handle for work in progress.
    pub fn get(&self, handle: i32) -> Option<Arc<T>> {
        self.slots[self.index(handle)?].value.clone()
    }

    /// Releasing a stale/invalid handle has no effect. No allocation is needed.
    pub fn remove(&mut self, handle: i32) -> Option<Arc<T>> {
        let index = self.index(handle)?;
        let slot = &mut self.slots[index];
        let value = slot.value.take()?;
        self.live -= 1;
        if slot.generation < GENERATION_MAX {
            slot.generation += 1;
            slot.next_free = self.free;
            self.free = Some(index);
        }
        Some(value)
    }

    pub fn len(&self) -> usize {
        self.live
    }
    pub fn is_empty(&self) -> bool {
        self.live == 0
    }

    fn index(&self, handle: i32) -> Option<usize> {
        if Kind::of(handle)? != self.kind {
            return None;
        }
        let raw = handle as u32;
        let index = (raw & INDEX_MASK) as usize;
        let generation = ((raw >> INDEX_BITS) & u32::from(GENERATION_MAX)) as u16;
        (self.slots.get(index)?.generation == generation).then_some(index)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_generation_never_wraps_back_to_a_stale_handle() {
        let mut slab = Slab::new(Kind::AudioData);
        let original = slab.insert(7).unwrap();
        let mut current = original;
        for _ in 0..=GENERATION_MAX {
            slab.remove(current).unwrap();
            current = slab.insert(9).unwrap();
            assert!(slab.get(original).is_none());
        }
        assert_ne!(current as u32 & INDEX_MASK, original as u32 & INDEX_MASK);
        assert_eq!(slab.len(), 1);
        assert_eq!(*slab.get(current).unwrap(), 9);
    }

    #[test]
    fn wrong_kind_and_repeated_release_cannot_remove_another_resource() {
        let mut audio = Slab::new(Kind::AudioData);
        let mut video = Slab::new(Kind::VideoFrame);
        let old = audio.insert(7).unwrap();
        let frame = video.insert(8).unwrap();
        assert!(video.get(old).is_none());
        assert!(audio.remove(frame).is_none());
        let retained = audio.get(old).unwrap();
        audio.remove(old);
        let new = audio.insert(9).unwrap();
        assert!(audio.remove(old).is_none());
        assert_eq!(*audio.get(new).unwrap(), 9);
        assert_eq!(*retained, 7);
        for invalid in [0, -1, i32::MAX] {
            assert!(audio.get(invalid).is_none());
        }
    }

    #[test]
    fn table_capacity_is_an_error_and_released_capacity_can_be_reused() {
        let mut slab = Slab::new(Kind::AudioData);
        let mut last = 0;
        for _ in 0..MAX_SLOTS {
            last = slab.insert_shared(Arc::new(())).unwrap();
        }
        assert_eq!(
            slab.insert(()).unwrap_err().kind,
            crate::ErrorKind::ResourceExhausted
        );
        slab.remove(last);
        assert!(slab.insert(()).is_ok());
    }
}
