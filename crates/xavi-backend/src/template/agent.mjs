// xavi's page agent. A program in a page opens media players, and each player is an audio element this agent keeps on the page's
// thread, where the elements' events fire. The host imports this module and calls `start({ memory, address })` with the program's
// shared memory and the address of its block: a mailbox, then 24 bytes on a slot of state for each player, 32 bytes each.
//
// The agent serves the mailbox, whose commands the generated wire (xavi_wire.mjs) runs against it (XaAgent). What the program
// reads of a player it reads from its slot, which this agent writes: an Int32 for the state (a PlaybackState's index: opening 0,
// paused 1, playing 2, buffering 3, ended 4) and one for the error (a MediaError.code, 0 for none), then three Float64s: the
// position and the duration, in seconds, and the volume, which the program owns.

import { Wire, serve } from "./xavi_wire.mjs";

const SLOTS = 24;
const WORDS = 32;
const PLAYERS = 32;

const OPENING = 0;
const PAUSED = 1;
const PLAYING = 2;
const BUFFERING = 3;
const ENDED = 4;

// The handle the wire names this agent by.
const AGENT = 1;

export function start({ memory, address }) {
  // The words of a slot. The memory may have grown, so the views are made when they are used.
  const words = (slot) => new Int32Array(memory.buffer, address + SLOTS + slot * WORDS, 2);
  const floats = (slot) => new Float64Array(memory.buffer, address + SLOTS + slot * WORDS + 8, 3);
  const state = (slot, value) => Atomics.store(words(slot), 0, value);
  const failed = (slot, code) => Atomics.store(words(slot), 1, code);
  const position = (slot, seconds) => (floats(slot)[0] = seconds);
  const duration = (slot, seconds) => (floats(slot)[1] = Number.isFinite(seconds) ? seconds : 0);

  // What is open: the element, whether the program wants it playing, and whether the browser refused a play for want of a
  // press, which is tried again at the next.
  const players = new Map();

  const play = (slot) => {
    const player = players.get(slot);
    if (!player) return;
    player.wanted = true;
    const started = player.audio.play();
    if (started && started.catch) {
      started.catch((e) => {
        if (e && e.name === "NotAllowedError") player.refused = true;
        else if (e && e.name !== "AbortError") failed(slot, player.audio.error?.code || 4);
      });
    }
  };

  // A press or a key is what a browser takes as the user's go-ahead for sound.
  const retry = () => {
    for (const [slot, player] of players) {
      if (player.refused && player.wanted) {
        player.refused = false;
        play(slot);
      }
    }
  };
  for (const type of ["pointerdown", "keydown", "touchend"]) {
    document.addEventListener(type, retry, { capture: true, passive: true });
  }

  const settle = (slot, audio) => {
    if (audio.ended) state(slot, ENDED);
    else if (audio.paused) state(slot, PAUSED);
    else if (audio.readyState < 3) state(slot, BUFFERING);
    else state(slot, PLAYING);
  };

  const agent = {
    open(slot, url) {
      agent.close(slot);
      const audio = new Audio();
      audio.preload = "auto";
      const player = { audio, wanted: false, refused: false };
      players.set(slot, player);
      const on = (type, handler) => audio.addEventListener(type, handler);
      on("loadedmetadata", () => {
        duration(slot, audio.duration);
        if (audio.paused && !player.wanted) state(slot, PAUSED);
      });
      on("durationchange", () => duration(slot, audio.duration));
      on("playing", () => state(slot, PLAYING));
      on("pause", () => {
        if (!audio.ended) state(slot, PAUSED);
      });
      on("waiting", () => state(slot, BUFFERING));
      on("canplay", () => {
        if (!player.wanted && audio.paused) state(slot, PAUSED);
        else if (!audio.paused) state(slot, PLAYING);
      });
      on("ended", () => {
        position(slot, audio.duration);
        state(slot, ENDED);
      });
      on("timeupdate", () => position(slot, audio.currentTime));
      on("seeked", () => {
        position(slot, audio.currentTime);
        settle(slot, audio);
      });
      on("error", () => failed(slot, audio.error?.code || 4));
      audio.src = url;
    },
    play,
    pause(slot) {
      const player = players.get(slot);
      if (!player) return;
      player.wanted = false;
      player.refused = false;
      player.audio.pause();
    },
    seek(slot, seconds) {
      const player = players.get(slot);
      if (player) player.audio.currentTime = seconds;
    },
    setVolume(slot, volume) {
      const player = players.get(slot);
      if (player) player.audio.volume = Math.min(1, Math.max(0, volume));
    },
    close(slot) {
      const player = players.get(slot);
      if (!player) return;
      players.delete(slot);
      player.audio.pause();
      player.audio.removeAttribute("src");
      player.audio.load();
    },
  };

  // The position of what plays moves smoothly between the element's coarse timeupdates.
  const frame = () => {
    for (const [slot, { audio }] of players) {
      if (!audio.paused && !audio.ended) position(slot, audio.currentTime);
    }
    requestAnimationFrame(frame);
  };
  requestAnimationFrame(frame);

  serve(new Wire(memory, new Map([[AGENT, agent]])), address);
}
