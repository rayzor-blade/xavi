use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};
use xavi_core::codec::AudioDecoderConfig;
use xavi_core::mux::{Engine, Mp4Config, MuxState, Muxer, Track};
use xavi_core::{EncodedChunk, EncodedChunkType, Error, ErrorKind, Result};

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "xavi-mux-test-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn output(&self) -> PathBuf {
        self.0.join("out.mp4")
    }
    fn empty(&self) -> bool {
        fs::read_dir(&self.0).unwrap().next().is_none()
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
struct Fake {
    file: fs::File,
    ready: bool,
    finishing: bool,
    ended: bool,
    fail: bool,
    fail_finish: bool,
}
impl Engine for Fake {
    fn open(path: &Path, config: &Mp4Config) -> Result<Self> {
        let name = &config.audio.as_ref().unwrap().codec;
        let file = fs::File::create_new(path).unwrap();
        if name == "open-error" {
            return Err(Error::unsupported("injected open failure"));
        }
        Ok(Self {
            file,
            ready: false,
            finishing: false,
            ended: false,
            fail: name == "write-error",
            fail_finish: name == "finish-error",
        })
    }
    fn write(
        &mut self,
        track: Track,
        chunk: &EncodedChunk,
        timestamp: i64,
        duration: u64,
    ) -> Result<bool> {
        if self.fail {
            return Err(Error::new(ErrorKind::Io, "injected write failure"));
        }
        if !self.ready {
            self.ready = true;
            return Ok(false);
        }
        assert_eq!(timestamp, 0);
        assert_eq!(
            duration,
            if track == Track::Audio {
                21_333
            } else {
                33_333
            }
        );
        use std::io::Write;
        self.file.write_all(chunk.bytes()).unwrap();
        Ok(true)
    }
    fn end_track(&mut self, _: Track) -> Result<()> {
        self.ended = true;
        Ok(())
    }
    fn finish(&mut self) -> Result<bool> {
        assert!(self.ended);
        if self.fail_finish {
            return Err(Error::new(ErrorKind::Io, "injected finalization failure"));
        }
        if !self.finishing {
            self.finishing = true;
            return Ok(false);
        }
        Ok(true)
    }
}
fn config(codec: &str) -> Mp4Config {
    Mp4Config {
        audio: Some(AudioDecoderConfig {
            codec: codec.into(),
            sample_rate: 48000,
            channels: 1,
            description: vec![17, 136].into(),
        }),
        video: None,
        timestamp_origin: -1000,
        default_video_duration: None,
    }
}
fn packet(timestamp: i64) -> EncodedChunk {
    EncodedChunk::new(EncodedChunkType::Key, timestamp, None, b"encoded").unwrap()
}
fn accept(writer: &mut Muxer<Fake>) {
    assert!(!writer.write(Track::Audio, &packet(-1000)).unwrap());
    assert!(writer.write(Track::Audio, &packet(-1000)).unwrap());
}

#[test]
fn incremental_writes_preserve_retries_and_publish_only_after_finalization() {
    let temp = Temp::new();
    let mut w = Muxer::<Fake>::create(temp.output(), config("aac"), 100).unwrap();
    assert!(!temp.output().exists());
    accept(&mut w);
    assert_eq!(
        w.write(Track::Audio, &packet(-1000)).unwrap_err().kind,
        ErrorKind::InvalidArgument
    );
    assert!(!w.finish().unwrap());
    assert_eq!(w.state(), MuxState::Finishing);
    assert!(!temp.output().exists());
    assert_eq!(
        w.write(Track::Audio, &packet(30000)).unwrap_err().kind,
        ErrorKind::InvalidState
    );
    assert!(w.finish().unwrap());
    assert!(w.finish().unwrap());
    w.close();
    drop(w);
    assert_eq!(fs::read(temp.output()).unwrap(), b"encoded");
    assert_eq!(fs::read_dir(&temp.0).unwrap().count(), 1);
}
#[test]
fn abandoned_and_failed_recordings_remove_their_partial_files() {
    let temp = Temp::new();
    {
        let mut w = Muxer::<Fake>::create(temp.output(), config("aac"), 100).unwrap();
        accept(&mut w);
    }
    assert!(temp.empty());
    assert!(Muxer::<Fake>::create(temp.output(), config("open-error"), 100).is_err());
    assert!(temp.empty());
    let mut w = Muxer::<Fake>::create(temp.output(), config("write-error"), 100).unwrap();
    assert!(w.write(Track::Audio, &packet(-1000)).is_err());
    assert_eq!(w.state(), MuxState::Failed);
    assert!(temp.empty());
    let mut w = Muxer::<Fake>::create(temp.output(), config("finish-error"), 100).unwrap();
    accept(&mut w);
    assert!(w.finish().is_err());
    assert_eq!(w.state(), MuxState::Failed);
    assert!(temp.empty());
}
#[test]
fn publication_cannot_overwrite_a_destination_created_during_recording() {
    let temp = Temp::new();
    let mut w = Muxer::<Fake>::create(temp.output(), config("aac"), 100).unwrap();
    accept(&mut w);
    fs::write(temp.output(), b"existing data").unwrap();
    assert!(!w.finish().unwrap());
    assert_eq!(w.finish().unwrap_err().kind, ErrorKind::Io);
    assert_eq!(fs::read(temp.output()).unwrap(), b"existing data");
    assert_eq!(fs::read_dir(&temp.0).unwrap().count(), 1);
    assert!(Muxer::<Fake>::create(temp.output(), config("aac"), 100).is_err());
}
#[test]
fn validation_errors_do_not_consume_track_state() {
    let temp = Temp::new();
    let mut w = Muxer::<Fake>::create(temp.output(), config("aac"), 7).unwrap();
    assert!(w.finish().is_err());
    assert!(w.end_track(Track::Audio).is_err());
    assert!(w.write(Track::Video, &packet(-1000)).is_err());
    assert!(w.write(Track::Audio, &packet(-1001)).is_err());
    assert!(w.write(Track::Audio, &packet(i64::MAX)).is_err());
    let big = EncodedChunk::new(EncodedChunkType::Key, -1000, None, b"too large").unwrap();
    assert!(w.write(Track::Audio, &big).is_err());
    let zero = EncodedChunk::new(EncodedChunkType::Key, -1000, Some(0), b"data").unwrap();
    assert!(w.write(Track::Audio, &zero).is_err());
    let duration = EncodedChunk::new(EncodedChunkType::Key, -1000, Some(10_000), b"data").unwrap();
    assert!(w.write(Track::Audio, &duration).is_err());
    accept(&mut w);
    w.end_track(Track::Audio).unwrap();
    w.end_track(Track::Audio).unwrap();
    assert!(w.write(Track::Audio, &packet(30000)).is_err());
    w.close();
    assert!(temp.empty());
}

#[test]
fn an_empty_second_track_can_be_filled_after_finish_is_rejected() {
    use xavi_core::codec::VideoDecoderConfig;
    let temp = Temp::new();
    let mut c = config("aac");
    c.video = Some(VideoDecoderConfig {
        codec: "avc1.42001E".into(),
        coded_width: Some(128),
        coded_height: Some(96),
        description: vec![1].into(),
        color_space: Default::default(),
    });
    c.default_video_duration = Some(33_333);
    let mut w = Muxer::<Fake>::create(temp.output(), c, 100).unwrap();
    accept(&mut w);
    assert!(w.finish().is_err());
    assert_eq!(w.state(), MuxState::Writing);
    let delta = EncodedChunk::new(EncodedChunkType::Delta, -1000, None, b"video").unwrap();
    assert!(w.write(Track::Video, &delta).is_err());
    assert!(w.write(Track::Video, &packet(-1000)).unwrap());
    w.end_track(Track::Audio).unwrap();
    assert!(w.write(Track::Audio, &packet(30_000)).is_err());
    assert!(!w.finish().unwrap());
    assert!(w.finish().unwrap());
    assert_eq!(fs::read(temp.output()).unwrap(), b"encodedencoded");
}
