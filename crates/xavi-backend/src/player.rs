//! File playback shared by runtime adapters. The native player owns the audio
//! sink and playback clock; callers upload returned VideoFrames to their GPU.
//! Apple currently requires these calls on the main event-loop thread.
use crate::{Error, ErrorKind, MediaBackend, Result};
use std::sync::Mutex;
pub use xavi_platform::player::{PlaybackInfo, PlaybackState, Player};

impl MediaBackend {
    pub fn player_open(&self, path: &str) -> Result<i32> {
        let player = Player::open(path)?;
        self.resources()?.players.insert(Mutex::new(player))
    }
    pub fn with_player<T>(
        &self,
        handle: i32,
        operation: impl FnOnce(&mut Player) -> Result<T>,
    ) -> Result<T> {
        let player = self
            .resources()?
            .players
            .get(handle)
            .ok_or_else(Error::closed)?;
        let mut player = player
            .lock()
            .map_err(|_| Error::new(ErrorKind::InvalidState, "player lock is poisoned"))?;
        operation(&mut player)
    }
    pub fn player_take_frame(&self, handle: i32) -> Result<i32> {
        let frame = self.with_player(handle, Player::take_frame)?;
        self.retain_video(frame)
    }
}
