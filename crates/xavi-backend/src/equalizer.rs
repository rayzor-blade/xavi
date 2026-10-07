//! Stateful DSP resources shared by generated runtime adapters.
use crate::{Error, ErrorKind, MediaBackend, Result};
use std::sync::{Arc, Mutex};
use xavi_core::equalizer::AudioEqualizer;

impl MediaBackend {
    pub fn equalizer_create(&self, bands: usize) -> Result<i32> {
        self.resources()?
            .equalizers
            .insert(Mutex::new(AudioEqualizer::new(bands)?))
    }
    pub fn with_equalizer<T>(
        &self,
        handle: i32,
        operation: impl FnOnce(&mut AudioEqualizer) -> Result<T>,
    ) -> Result<T> {
        let resource = self
            .resources()?
            .equalizers
            .get(handle)
            .ok_or_else(Error::closed)?;
        let mut eq = resource
            .lock()
            .map_err(|_| Error::new(ErrorKind::InvalidState, "equalizer lock is poisoned"))?;
        operation(&mut eq)
    }
    pub fn equalizer_process(&self, handle: i32, audio: i32) -> Result<i32> {
        let input = self.audio(audio)?;
        let output = self.with_equalizer(handle, |eq| eq.process(&input))?;
        self.retain_audio(Arc::new(output))
    }
}
