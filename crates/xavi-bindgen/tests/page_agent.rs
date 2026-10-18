//! The page agent end to end, under Node with a stand-in `Audio`: the commands the generated Rust encodes reach the player
//! through the mailbox, and what the element does comes back in the slot the program reads. Needs `rustc` and `node`.

use std::path::Path;
use std::process::Command;

/// Encodes the commands of each step, as the program sends them.
const PROGRAM: &str = r#"
#[allow(dead_code, non_camel_case_types, clippy::all)]
mod wire {
    include!("wire.rs");
}
use wire::*;

fn main() {
    let agent = Handle(1);
    let mut e = Encoder::new();
    e.xa_agent_open(agent, &0, &"a.mp3".to_owned());
    e.xa_agent_play(agent, &0);
    e.xa_agent_set_volume(agent, &0, &0.5);
    std::fs::write("open.bin", &e.bytes).unwrap();
    let mut e = Encoder::new();
    e.xa_agent_seek(agent, &0, &30.0);
    std::fs::write("seek.bin", &e.bytes).unwrap();
    let mut e = Encoder::new();
    e.xa_agent_pause(agent, &0);
    std::fs::write("pause.bin", &e.bytes).unwrap();
    let mut e = Encoder::new();
    e.xa_agent_close(agent, &0);
    std::fs::write("close.bin", &e.bytes).unwrap();
}
"#;

const HARNESS: &str = r#"
import { readFileSync } from "node:fs";

const made = [];
// An audio element as far as the agent uses one. The first play is refused, as a browser does before a press.
class Audio extends EventTarget {
  constructor() {
    super();
    this.paused = true;
    this.ended = false;
    this.currentTime = 0;
    this.duration = NaN;
    this.readyState = 0;
    this.volume = 1;
    this.error = null;
    this.src = "";
    this.refusals = made.length === 0 ? 1 : 0;
    this.log = [];
    made.push(this);
  }
  play() {
    this.log.push("play");
    if (this.refusals > 0) {
      this.refusals--;
      return Promise.reject(Object.assign(new Error("blocked"), { name: "NotAllowedError" }));
    }
    this.paused = false;
    this.readyState = 4;
    queueMicrotask(() => this.dispatchEvent(new Event("playing")));
    return Promise.resolve();
  }
  pause() {
    this.log.push("pause");
    this.paused = true;
    this.dispatchEvent(new Event("pause"));
  }
  removeAttribute(name) { this.log.push("remove " + name); }
  load() { this.log.push("load"); }
}
globalThis.Audio = Audio;
const page = new EventTarget();
globalThis.document = page;
globalThis.requestAnimationFrame = (frame) => setTimeout(frame, 1);

const memory = { buffer: new SharedArrayBuffer(1 << 20) };
const words = new Int32Array(memory.buffer);
const floats = new Float64Array(memory.buffer);
// Slot 0: the state and the error at 24 and 28, the position, duration and volume at 32, 40 and 48.
const slot = () => ({ state: words[6], error: words[7], position: floats[4], duration: floats[5] });
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

const { start } = await import("./xavi.mjs");
start({ memory, address: 0 });

const send = async (file) => {
  const batch = readFileSync(file);
  new Uint8Array(memory.buffer, 1 << 17, batch.length).set(batch);
  words.set([1 << 17, batch.length], 2);
  const serial = Atomics.add(words, 0, 1) + 1;
  Atomics.notify(words, 0);
  while (Atomics.load(words, 1) !== serial) await sleep(1);
};
const fire = (audio, type) => audio.dispatchEvent(new Event(type));
const steps = [];

// Opened, played and given a volume: the element is made and told to play, the browser refuses, the state is still opening.
await send("open.bin");
const audio = made[0];
steps.push(["opened", audio.src, audio.volume, slot().state, audio.log.join(",")]);
audio.duration = 120;
fire(audio, "loadedmetadata");
steps.push(["metadata", slot().duration, slot().state]);
// A press: the refused play is made again and the element plays.
page.dispatchEvent(new Event("pointerdown"));
await sleep(20);
steps.push(["after a press", slot().state, audio.log.join(",")]);
// It plays on: the position follows its time between timeupdates, too.
audio.currentTime = 5;
await sleep(20);
steps.push(["playing", slot().position]);
// Seeked.
await send("seek.bin");
fire(audio, "seeked");
steps.push(["seeked", audio.currentTime, slot().position, slot().state]);
// Paused.
await send("pause.bin");
steps.push(["paused", slot().state]);
// A file that cannot be played.
audio.error = { code: 4 };
fire(audio, "error");
steps.push(["error", slot().error]);
// Ended.
audio.error = null;
audio.ended = true;
audio.currentTime = 120;
fire(audio, "ended");
steps.push(["ended", slot().state, slot().position]);
// Closed.
await send("close.bin");
steps.push(["closed", audio.log.slice(-3).join(",")]);
console.log(JSON.stringify(steps));
process.exit(0);
"#;

#[test]
fn commands_reach_the_player_and_its_state_comes_back() {
    let wire = xavi_bindgen::browser_wire().unwrap();
    let dir = std::env::temp_dir().join(format!("xavi-page-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("wire.rs"), &wire.rust).unwrap();
    std::fs::write(dir.join(xavi_backend::WIRE_MODULE), &wire.js).unwrap();
    std::fs::write(dir.join(xavi_backend::AGENT_MODULE), xavi_backend::AGENT).unwrap();
    std::fs::write(dir.join("main.rs"), PROGRAM).unwrap();
    std::fs::write(dir.join("harness.mjs"), HARNESS).unwrap();
    run(
        &dir,
        Command::new("rustc").args(["--edition", "2024", "-O", "main.rs", "-o", "program"]),
    );
    run(&dir, &mut Command::new(dir.join("program")));
    let out = run(&dir, Command::new("node").arg("harness.mjs"));
    std::fs::remove_dir_all(&dir).ok();

    assert_eq!(
        out.trim(),
        concat!(
            r#"[["opened","a.mp3",0.5,0,"play"],"#,
            r#"["metadata",120,0],"#,
            r#"["after a press",2,"play,play"],"#,
            r#"["playing",5],"#,
            r#"["seeked",30,30,2],"#,
            r#"["paused",1],"#,
            r#"["error",4],"#,
            r#"["ended",4,120],"#,
            r#"["closed","pause,remove src,load"]]"#
        )
    );
}

fn run(dir: &Path, command: &mut Command) -> String {
    let out = command.current_dir(dir).output().expect("the tool runs");
    assert!(
        out.status.success(),
        "{:?}: {}{}",
        command,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}
