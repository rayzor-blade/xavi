//! A media player in a page: an HTMLMediaElement that an agent (agent.mjs), running on the page's thread, keeps for the
//! program. The program asks its host to start the `xavi` agent with the address of a block: a mailbox, then a slot of
//! state for each player. What the program asks of a player, to open a file, play, pause, seek, set the volume or close,
//! is encoded onto the wire (`crate::wire`) and sent through the mailbox, and a send waits until the agent has run it.
//! What the program reads of one, its state, position, duration and volume, is in its slot, which the agent writes as the
//! element's events and the page's animation frames come, so reading is not a call.
//!
//! Only the player is in a page so far. The rest of xavi, media data, codecs, queues and muxers, raises that it is not
//! available. A player plays audio and has no video frames yet: `pollFrame` is false and there is no equalizer.

use std::sync::atomic::{AtomicI32, AtomicU64, Ordering::SeqCst};
use std::sync::{LazyLock, Mutex, MutexGuard};

use xavi_core::handles::{Kind, Slab};

use crate::runtime::{ErrorKind, Text, host};
use crate::wire::{self, Handle, Mailbox};
use crate::*;

/// The name a host starts the agent by: it imports `xavi.mjs` beside the program and calls its `start({ memory, address })`.
const AGENT_NAME: &str = "xavi";

/// The agent, which the wire names by this handle.
const AGENT: Handle = Handle(1);

/// How many players a page has open at once.
const PLAYERS: usize = 32;

/// What the agent keeps of a player. Its state is a `PlaybackState`'s index and its error a `MediaError.code`, or 0; the
/// position, duration and volume are `f64`s, as their bits. The agent writes all but the volume, which is the program's.
#[repr(C)]
struct Slot {
    state: AtomicI32,
    error: AtomicI32,
    position: AtomicU64,
    duration: AtomicU64,
    volume: AtomicU64,
}

impl Slot {
    const fn new() -> Self {
        Self {
            state: AtomicI32::new(0),
            error: AtomicI32::new(0),
            position: AtomicU64::new(0),
            duration: AtomicU64::new(0),
            volume: AtomicU64::new(0),
        }
    }

    fn reset(&self) {
        self.state.store(0, SeqCst);
        self.error.store(0, SeqCst);
        self.position.store(0f64.to_bits(), SeqCst);
        self.duration.store(0f64.to_bits(), SeqCst);
        self.volume.store(1f64.to_bits(), SeqCst);
    }

    fn get(word: &AtomicU64) -> f64 {
        f64::from_bits(word.load(SeqCst))
    }
}

/// What the agent is started with. The agent reads the mailbox at its address and the slots 24 bytes on.
#[repr(C)]
struct Block {
    mailbox: Mailbox,
    slots: [Slot; PLAYERS],
}

static BLOCK: Block = Block {
    mailbox: Mailbox::new(),
    slots: [const { Slot::new() }; PLAYERS],
};

struct Page {
    players: Slab<()>,
    /// Whether the host started the agent, which then serves the page for the program's life.
    started: bool,
    commands: wire::Encoder,
}

static PAGE: LazyLock<Mutex<Page>> = LazyLock::new(|| {
    Mutex::new(Page {
        players: Slab::new(Kind::MediaPlayer),
        started: false,
        commands: wire::Encoder::new(),
    })
});

fn page() -> MutexGuard<'static, Page> {
    PAGE.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl Page {
    /// Hands the commands to the agent and waits until it has run them.
    fn flush(&mut self) {
        if !self.commands.bytes.is_empty() {
            BLOCK.mailbox.send(&self.commands.bytes);
            self.commands.bytes.clear();
        }
    }
}

/// The slot a handle names, or a raised error and `None` when it names none: it was closed, or never was.
fn slot_of(p: &Page, handle: i32) -> Option<usize> {
    if p.players.get(handle).is_none() {
        host::raise(ErrorKind::Runtime, "the player is closed");
        return None;
    }
    Some((handle as u32 & 0xffff) as usize)
}

/// What a `MediaError.code` says, for a program that was not there to see the page's console.
fn failure(code: i32) -> &'static str {
    match code {
        2 => "the file could not be fetched",
        3 => "the file could not be decoded",
        4 => "this browser cannot play that file, or there is none at that address",
        _ => "playback was aborted",
    }
}

pub unsafe fn player_open(path: Text) -> i32 {
    let mut p = page();
    if !p.started {
        if !host::agent(AGENT_NAME, &BLOCK as *const Block as usize) {
            host::raise(
                ErrorKind::Runtime,
                "this program's host starts no media agent",
            );
            return 0;
        }
        p.started = true;
    }
    let Ok(handle) = p.players.insert(()) else {
        host::raise(ErrorKind::Runtime, "too many players are open");
        return 0;
    };
    let slot = (handle as u32 & 0xffff) as usize;
    if slot >= PLAYERS {
        p.players.remove(handle);
        host::raise(ErrorKind::Runtime, "too many players are open");
        return 0;
    }
    BLOCK.slots[slot].reset();
    let url = path.as_str().to_owned();
    p.commands.xa_agent_open(AGENT, &(slot as u32), &url);
    p.flush();
    handle
}

pub unsafe fn player_state(this: i32) -> i32 {
    let p = page();
    let Some(slot) = slot_of(&p, this) else {
        return PlaybackState::Ended.native();
    };
    let slot = &BLOCK.slots[slot];
    let error = slot.error.load(SeqCst);
    if error != 0 {
        host::raise(
            ErrorKind::Runtime,
            &format!("could not play: {}", failure(error)),
        );
        return PlaybackState::Ended.native();
    }
    match slot.state.load(SeqCst) {
        1 => PlaybackState::Paused,
        2 => PlaybackState::Playing,
        3 => PlaybackState::Buffering,
        4 => PlaybackState::Ended,
        _ => PlaybackState::Opening,
    }
    .native()
}

pub unsafe fn player_position(this: i32) -> f64 {
    let p = page();
    slot_of(&p, this).map_or(0.0, |s| Slot::get(&BLOCK.slots[s].position))
}

pub unsafe fn player_duration(this: i32) -> f64 {
    let p = page();
    slot_of(&p, this).map_or(0.0, |s| Slot::get(&BLOCK.slots[s].duration))
}

pub unsafe fn player_volume(this: i32) -> f64 {
    let p = page();
    slot_of(&p, this).map_or(0.0, |s| Slot::get(&BLOCK.slots[s].volume))
}

pub unsafe fn player_play(this: i32) {
    let mut p = page();
    if let Some(slot) = slot_of(&p, this) {
        p.commands.xa_agent_play(AGENT, &(slot as u32));
        p.flush();
    }
}

pub unsafe fn player_pause(this: i32) {
    let mut p = page();
    if let Some(slot) = slot_of(&p, this) {
        p.commands.xa_agent_pause(AGENT, &(slot as u32));
        p.flush();
    }
}

pub unsafe fn player_seek(this: i32, seconds: f64) {
    let mut p = page();
    if let Some(slot) = slot_of(&p, this) {
        // Read back as the target at once; the agent moves the element, and writes the position as it plays on from there.
        BLOCK.slots[slot]
            .position
            .store(seconds.max(0.0).to_bits(), SeqCst);
        p.commands.xa_agent_seek(AGENT, &(slot as u32), &seconds.max(0.0));
        p.flush();
    }
}

pub unsafe fn player_set_volume(this: i32, volume: f64) {
    let mut p = page();
    if let Some(slot) = slot_of(&p, this) {
        let volume = if volume.is_finite() { volume.clamp(0.0, 1.0) } else { 1.0 };
        BLOCK.slots[slot].volume.store(volume.to_bits(), SeqCst);
        p.commands.xa_agent_set_volume(AGENT, &(slot as u32), &volume);
        p.flush();
    }
}

/// A player in a page has no video frames to hand over.
pub unsafe fn player_poll_frame(this: i32) -> bool {
    let p = page();
    slot_of(&p, this);
    false
}

/// Nothing is filtered in a page, so nothing is to be cleared.
pub unsafe fn player_clear_equalizer(this: i32) {
    let p = page();
    slot_of(&p, this);
}

pub unsafe fn player_close(this: i32) {
    let mut p = page();
    let Some(slot) = slot_of(&p, this) else {
        return;
    };
    p.commands.xa_agent_close(AGENT, &(slot as u32));
    p.flush();
    p.players.remove(this);
}
