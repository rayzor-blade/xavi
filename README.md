# xavi

Cross-platform audio and video for Ash, Rayzor, and Caribou. xavi provides
native playback, codecs, streaming queues, media editing, MP4 containers, and
an equalizer behind one IDL-defined API.

## Use it from Haxe

[hlavi](https://github.com/rayzor-blade/hlavi) packages the generated API and
native implementation for Ash and HashLink. Install `hlavi.zip` from its
[releases](https://github.com/rayzor-blade/hlavi/releases), along with the
`ash-future` package from [Ash](https://github.com/rayzor-blade/ash/releases).
Applications need only Haxe and their runtime; `NativeInstall` downloads the
matching native library during compilation.

```sh
haxelib install /path/to/ash-future.zip
haxelib install /path/to/hlavi.zip
haxe -lib hlavi -main Main -hl main.hl
ash main.hl
```

For example, `Main.hx` can create a video frame and process audio:

```haxe
import haxe.Int64;
import haxe.io.Bytes;
import media.*;

class Main {
    static function main() {
        // A 2 × 2 opaque red RGBA frame.
        var pixels = Bytes.alloc(16);
        for (i in 0...4) {
            pixels.set(i * 4, 255);
            pixels.set(i * 4 + 3, 255);
        }
        var frame = VideoFrame.create(pixels, new VideoFrameBufferInit(
            VideoPixelFormat.RGBA,
            Int64.ofInt(2), Int64.ofInt(2), Int64.ofInt(0)
        ));
        trace(frame.codedWidth());
        frame.close();

        // A quiet 440 Hz mono tone, sampled at 48 kHz.
        var pcm = Bytes.alloc(1024 * 4);
        for (i in 0...1024)
            pcm.setFloat(i * 4, 0.1 * Math.sin(2 * Math.PI * 440 * i / 48000));
        var audio = AudioData.create(new AudioDataInit(
            AudioSampleFormat.F32, 48000,
            Int64.ofInt(1024), Int64.ofInt(1), Int64.ofInt(0), pcm
        ));
        var eq = AudioEqualizer.create(1);
        eq.setBand(0, 440, 6, 1); // Band index, frequency, gain in dB, Q.
        eq.setPreamp(-6);
        var filtered = eq.process(audio);
        trace(filtered.numberOfFrames());
        filtered.close();
        audio.close();
        eq.close();
    }
}
```

These operations produce owned media buffers. To display video and hear its
sound, use the [Haxe video player](https://github.com/rayzor-blade/hlavi/tree/main/examples/player):
hlavi handles decoding and audio output, hlwgpu renders the frames, and
hlwindow supplies the window and input. It supports pause, seeking, volume,
fullscreen, and a live equalizer without application Rust or C code.

With an open `MediaPlayer`, attach an equalizer directly to playback:

```haxe
var eq = AudioEqualizer.create(3);
eq.setBand(0, 100, 6, 0.7);
eq.setBand(1, 1000, -2, 1);
eq.setBand(2, 8000, 3, 0.7);
eq.setPreamp(-9);
player.setEqualizer(eq);
eq.close(); // Playback keeps a copy of the settings and its own filter history.
// player.clearEqualizer() restores flat playback.
```

## What the API provides

| API | Operations |
|---|---|
| `MediaPlayer` | File playback with synchronized audio, polled video frames, pause, seek, volume, and EQ |
| `VideoFrame` | CPU pixel storage, layouts and metadata, copying, crop, resize, blend, retiming |
| `AudioData` | PCM storage, format conversion, copying, slice, gain, mix, retiming |
| `AudioEqualizer` | 1–16 peaking bands, preamp, bypass, continuous PCM processing and live playback |
| `MediaQueue` | Bounded PCM, frame, encoded-chunk, and byte queues with backpressure |
| `AudioEncoder` / `VideoEncoder` | Incremental native encoding and decoder configuration |
| `AudioDecoder` / `VideoDecoder` | Incremental native decoding, polling and flush |
| `MediaDemuxer` / `MediaMuxer` | MP4 packet reading and native MP4 writing |

Streaming means incremental processing with bounded memory and explicit
backpressure; xavi does not provide a network transport or HLS/DASH client.
Editing operates on media buffers and packets; there is no timeline editor.
The current codec profile is AAC-LC and H.264 Baseline, with MP4 container
support. Device capture, browser media, and GPU-backed frames remain future
work. The IDL describes some capabilities beyond the implemented native
surface; [the Rust API declaration](api/media.api.rs) selects what is generated.

Timestamps and durations are in microseconds. Haxe uses `Int64` for ABI
counts and timestamps. Resources must be explicitly closed; wrapper garbage
collection does not release native handles. Returned snapshots own their
storage, and clones remain valid after closing the original.

## Platforms

| Platform | Frameworks | Release build targets |
|---|---|---|
| macOS | AVFoundation, AudioToolbox, VideoToolbox, MediaToolbox | ARM64, x86_64 |
| iOS | The same Apple frameworks | ARM64 device and ARM64 simulator |
| Windows | Media Foundation and Media Engine | x86_64 |
| Android | NDK MediaCodec, MediaMuxer, AAudio | ARM64, ARMv7, x86_64; API 28+ |
| Linux | System GStreamer 1.20+ | ARM64, x86_64 |

Linux needs GStreamer's appsrc/appsink, converters, parsers, and suitable
non-FFmpeg plugins. The current codec path uses `voaacenc`, `faad` or
`fdkaacdec`, `openh264enc`, and a VA or OpenH264 decoder. Playback also needs
an audio output plugin. Availability depends on the system; query codec
support and handle unsupported configurations.

No FFmpeg dependency or automatic fallback is included. Any future FFmpeg
backend must be explicitly enabled. Playback and EQ have been exercised on
macOS, Linux, Android, and the iOS simulator; Windows has compile validation.
CI compiles every listed target, runs CPU/ABI tests on desktop hosts, and
runs native codec integration tests on macOS. CI compilation alone does not
establish device playback support.

## Versioned and nightly SDK packages

[xavi releases](https://github.com/rayzor-blade/xavi/releases) are shared by
runtime adapters and other Rust consumers:

| Asset | Contents |
|---|---|
| `xavi-sdk.zip` | Portable Rust source SDK: all xavi crates, IDL, tests, examples, lockfiles, and the pinned x-idl generator source |
| `xavi-tools-<platform>.zip` | Prebuilt `xavi-haxe` generator and generated Ash/HashLink and Rayzor Haxe sources |
| `SHA256SUMS` | SHA-256 checksums for every ZIP |

Host tool packages cover `macos-aarch64`, `macos-x86_64`, `linux-aarch64`,
`linux-x86_64`, and `windows-x86_64`. The same SDK supports all desktop and
mobile targets above. Runtime adapters compile its crates for their target
and ship the resulting runtime library. The SDK is a source distribution;
it does not promise a portable Rust binary ABI or include a VM-specific HDLL.
Rust registry dependencies are resolved using `Cargo.lock` and still require
network access or a populated Cargo cache.

Unpack the SDK beside your adapter:

```text
workspace/
  my-adapter/
  xavi/
  x-idl/
  xavi-sdk.json
```

The SDK manifest records its xavi revision, bundled x-idl revision, supported
targets, and checksums of every source file. For a Rust consumer:

```toml
[dependencies]
xavi-core = { path = "../xavi/crates/xavi-core" }
xavi-backend = { path = "../xavi/crates/xavi-backend" }

[build-dependencies]
xavi-backend = { path = "../xavi/crates/xavi-backend" }
xavi-bindgen = { path = "../xavi/crates/xavi-bindgen" }
```

To generate Haxe with a host tool package:

```sh
bin/xavi-haxe ash generated/ash
bin/xavi-haxe rayzor generated/rayzor
```

The [release workflow](.github/workflows/release.yml) publishes immutable
`v*` releases and updates `nightly` after successful main-branch or scheduled
builds. All ten native targets must build from the packaged SDK before
publishing. A manual run with an empty `release_tag` validates and uploads
workflow artifacts without publishing. For a manual version release, select
the matching existing tag. No release is downloadable until its publishing
run completes.

hlavi consumes this SDK release, verifies its checksums and pinned revision,
and builds its own desktop/mobile adapters. Haxe users install hlavi packages;
they do not download or compile the Rust SDK themselves.

## Architecture and development

- `xavi-core`: runtime-independent buffers, editing, equalizer DSP, streams,
  codec lifecycle, resource handles, and muxing contracts.
- `xavi-platform`: native framework codecs, playback/audio output, and muxers.
- `xavi-backend`: shared operations, handle tables, queues, demuxing, and the
  native adapter template.
- `xavi-bindgen`: x-idl generation for HashLink/Ash, Rayzor, and Caribou.
- `xavi-check`: generated ABI integration tests using a Rust test host.

The media contract lives in [media.idl](api/spec/media.idl) and
[media.api.rs](api/media.api.rs). Runtime adapters call
`xavi_bindgen::generate` and `xavi_backend::install` from their build scripts,
supply the runtime carriers and a `with_media` context hook, and include the
generated files. [hlavi](https://github.com/rayzor-blade/hlavi) is the current
Ash/HashLink adapter. Generator support for Rayzor and Caribou does not yet
constitute a packaged runtime integration for either.

For development, keep `x-idl` beside the xavi checkout. Its release revision
is recorded in [release-sources.json](release-sources.json). Use a recent
stable Rust toolchain, Python 3.11+, and the platform SDK/toolchain:

```sh
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo run -p xavi-bindgen --bin xavi-haxe -- ash target/haxe
cargo run -p xavi-backend --example record -- target/recording.mp4
cargo run -p xavi-platform --example playback -- target/recording.mp4
```

Native codec tests require installed codecs and access to platform services.
For CPU-only data and generation checks, run
`cargo test -p xavi-core -p xavi-bindgen -p xavi-check --locked`.
Android cross builds use `cargo-ndk` and NDK r27 with API level 28; Apple
builds use the matching Xcode SDK. FFmpeg is not needed for these builds.
