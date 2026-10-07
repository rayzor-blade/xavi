//! Bounded runtime-facing streams. VM objects never leave the calling thread.
//! Native codecs are created, driven, and destroyed on their own worker.
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::thread::{self, JoinHandle};
use std::time::Duration;
use xavi_core::codec::{self, Engine};
use xavi_core::stream::{self, Limits, Payload, Read, Receiver, Sender};
use xavi_core::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaKind {
    Audio,
    Video,
    AudioChunk,
    VideoChunk,
    Bytes,
}
#[derive(Clone)]
pub enum Item {
    Audio(Arc<AudioData>),
    Video(Arc<VideoFrame>),
    AudioChunk(Arc<EncodedChunk>),
    VideoChunk(Arc<EncodedChunk>),
    Bytes(Arc<[u8]>),
}
impl Item {
    pub fn kind(&self) -> MediaKind {
        match self {
            Self::Audio(_) => MediaKind::Audio,
            Self::Video(_) => MediaKind::Video,
            Self::AudioChunk(_) => MediaKind::AudioChunk,
            Self::VideoChunk(_) => MediaKind::VideoChunk,
            Self::Bytes(_) => MediaKind::Bytes,
        }
    }
}
impl Payload for Item {
    fn payload_bytes(&self) -> usize {
        match self {
            Self::Audio(v) => v.bytes().len(),
            Self::Video(v) => v.bytes().len(),
            Self::AudioChunk(v) | Self::VideoChunk(v) => v.bytes().len(),
            Self::Bytes(v) => v.len(),
        }
    }
}
#[derive(Clone, Debug)]
pub enum Configuration {
    Audio(codec::AudioDecoderConfig),
    Video(codec::VideoDecoderConfig),
}
impl Configuration {
    pub fn description(&self) -> &[u8] {
        match self {
            Self::Audio(c) => &c.description,
            Self::Video(c) => &c.description,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Pending,
    Ready,
    Ended,
}

pub(crate) fn lock<T>(mutex: &Mutex<T>) -> Result<std::sync::MutexGuard<'_, T>> {
    mutex
        .lock()
        .map_err(|_| Error::new(ErrorKind::InvalidState, "media worker lock is poisoned"))
}
pub fn limits(items: i32, bytes: i64) -> Result<Limits> {
    if !(1..=1024).contains(&items) || !(1..=256 * 1024 * 1024).contains(&bytes) {
        return Err(Error::invalid(
            "queue limits must be 1..=1024 items and 1..=256 MiB",
        ));
    }
    Ok(Limits {
        max_items: items as usize,
        max_bytes: bytes as usize,
    })
}
pub struct Reader<T: Payload> {
    receiver: Receiver<T>,
    pending: Option<T>,
    ended: bool,
}
impl<T: Payload> Reader<T> {
    pub fn new(receiver: Receiver<T>) -> Self {
        Self {
            receiver,
            pending: None,
            ended: false,
        }
    }
    pub fn status(&mut self) -> Result<Status> {
        if self.pending.is_some() {
            return Ok(Status::Ready);
        }
        if self.ended {
            return Ok(Status::Ended);
        }
        match self.receiver.try_next()? {
            Read::Item(v) => {
                self.pending = Some(v);
                Ok(Status::Ready)
            }
            Read::Pending => Ok(Status::Pending),
            Read::End => {
                self.ended = true;
                Ok(Status::Ended)
            }
        }
    }
    pub fn take(&mut self) -> Result<T> {
        if self.status()? != Status::Ready {
            return Err(Error::new(
                ErrorKind::WouldBlock,
                "poll must report ready before reading",
            ));
        }
        Ok(self.pending.take().unwrap())
    }
    pub fn peek(&mut self) -> Result<&T> {
        if self.status()? != Status::Ready {
            return Err(Error::new(
                ErrorKind::WouldBlock,
                "poll must report ready before reading",
            ));
        }
        Ok(self.pending.as_ref().unwrap())
    }
}
pub fn send<T: Payload>(sender: &mut Sender<T>, value: T) -> Result<bool> {
    match sender.try_send(value) {
        Ok(()) => Ok(true),
        Err(e) if e.error.kind == ErrorKind::WouldBlock => Ok(false),
        Err(e) => Err(e.error),
    }
}
pub struct Queue {
    pub kind: MediaKind,
    pub input: Mutex<Sender<Item>>,
    pub output: Mutex<Reader<Item>>,
}
impl Queue {
    pub fn new(kind: MediaKind, limits: Limits) -> Result<Self> {
        let (input, output) = stream::channel(limits)?;
        Ok(Self {
            kind,
            input: Mutex::new(input),
            output: Mutex::new(Reader::new(output)),
        })
    }
    pub fn write(&self, value: Item) -> Result<bool> {
        if value.kind() != self.kind {
            return Err(Error::invalid("wrong payload type for this queue"));
        }
        send(&mut *lock(&self.input)?, value)
    }
    pub fn finish(&self) -> Result<()> {
        lock(&self.input)?.finish()
    }
}

pub struct Worker {
    pub cancelled: Arc<AtomicBool>,
    pub error: Arc<Mutex<Option<Error>>>,
    join: Option<JoinHandle<()>>,
}
impl Worker {
    pub fn start<F>(name: &str, work: F) -> Result<Self>
    where
        F: FnOnce(Arc<AtomicBool>, mpsc::SyncSender<Result<()>>) -> Result<()> + Send + 'static,
    {
        let cancelled = Arc::new(AtomicBool::new(false));
        let error = Arc::new(Mutex::new(None));
        let cancel = cancelled.clone();
        let failure = error.clone();
        let (tx, rx) = mpsc::sync_channel(1);
        let join = thread::Builder::new()
            .name(name.into())
            .spawn(move || {
                let result =
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| work(cancel, tx)))
                        .unwrap_or_else(|_| {
                            Err(Error::new(ErrorKind::InvalidState, "media worker panicked"))
                        });
                if let Err(e) = result
                    && let Ok(mut slot) = failure.lock()
                {
                    *slot = Some(e);
                }
            })
            .map_err(|e| Error::new(ErrorKind::ResourceExhausted, e.to_string()))?;
        let worker = Self {
            cancelled,
            error,
            join: Some(join),
        };
        rx.recv().map_err(|_| {
            Error::new(
                ErrorKind::InvalidState,
                "media worker initialization failed",
            )
        })??;
        Ok(worker)
    }
    pub fn check(&self) -> Result<()> {
        match lock(&self.error)?.as_ref() {
            Some(e) => Err(e.clone()),
            None => Ok(()),
        }
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Release);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

pub struct Session {
    pub input_kind: MediaKind,
    pub output_kind: MediaKind,
    input: Mutex<Sender<Item>>,
    pub output: Mutex<Reader<Item>>,
    metadata: Arc<Mutex<Option<Configuration>>>,
    pub worker: Worker,
}
impl Session {
    pub fn audio_encoder(
        config: codec::AudioEncoderConfig,
        input: Limits,
        output: Limits,
    ) -> Result<Self> {
        Self::start::<xavi_platform::AudioEncoder>(
            config,
            input,
            output,
            MediaKind::Audio,
            MediaKind::AudioChunk,
            |item| match item {
                Item::Audio(a) => Ok(a),
                _ => Err(Error::invalid("expected audio")),
            },
            |o| {
                (
                    Item::AudioChunk(o.chunk),
                    o.decoder_config.map(Configuration::Audio),
                )
            },
        )
    }
    pub fn video_encoder(
        config: codec::VideoEncoderConfig,
        input: Limits,
        output: Limits,
    ) -> Result<Self> {
        Self::start::<xavi_platform::VideoEncoder>(
            config,
            input,
            output,
            MediaKind::Video,
            MediaKind::VideoChunk,
            |item| match item {
                Item::Video(frame) => Ok(codec::VideoEncodeInput {
                    frame,
                    key_frame: true,
                }),
                _ => Err(Error::invalid("expected video")),
            },
            |o| {
                (
                    Item::VideoChunk(o.chunk),
                    o.decoder_config.map(Configuration::Video),
                )
            },
        )
    }
    pub fn audio_decoder(
        config: codec::AudioDecoderConfig,
        input: Limits,
        output: Limits,
    ) -> Result<Self> {
        Self::start::<xavi_platform::AudioDecoder>(
            config,
            input,
            output,
            MediaKind::AudioChunk,
            MediaKind::Audio,
            |item| match item {
                Item::AudioChunk(a) => Ok(a),
                _ => Err(Error::invalid("expected audio chunk")),
            },
            |o| (Item::Audio(o), None),
        )
    }
    pub fn video_decoder(
        config: codec::VideoDecoderConfig,
        input: Limits,
        output: Limits,
    ) -> Result<Self> {
        Self::start::<xavi_platform::VideoDecoder>(
            config,
            input,
            output,
            MediaKind::VideoChunk,
            MediaKind::Video,
            |item| match item {
                Item::VideoChunk(a) => Ok(a),
                _ => Err(Error::invalid("expected video chunk")),
            },
            |o| (Item::Video(o), None),
        )
    }
    #[allow(clippy::too_many_arguments)]
    fn start<E>(
        config: E::Config,
        input_limits: Limits,
        output_limits: Limits,
        input_kind: MediaKind,
        output_kind: MediaKind,
        decode: fn(Item) -> Result<E::Input>,
        encode: fn(E::Output) -> (Item, Option<Configuration>),
    ) -> Result<Self>
    where
        E: Engine + 'static,
        E::Config: Send + 'static,
    {
        let (input, mut source) = stream::channel::<Item>(input_limits)?;
        let (mut sink, output) = stream::channel::<Item>(output_limits)?;
        let metadata = Arc::new(Mutex::new(None));
        let meta = metadata.clone();
        let worker = Worker::start("xavi-codec", move |cancel, initialized| {
            let mut codec = codec::Codec::<E>::new(input_limits, output_limits)?;
            let result = codec.configure(config);
            let _ = initialized.send(result.clone());
            result?;
            let result = (|| {
                let mut pending_input = None;
                let mut pending_output = None;
                let mut eof = false;
                let mut flush = None;
                while !cancel.load(Ordering::Acquire) {
                    if let Some(value) = pending_output.take() {
                        match sink.try_send(value) {
                            Ok(()) => {}
                            Err(e) if e.error.kind == ErrorKind::WouldBlock => {
                                pending_output = Some(e.value);
                                thread::sleep(Duration::from_millis(1));
                                continue;
                            }
                            Err(e) => return Err(e.error),
                        }
                    }
                    if pending_input.is_none() && !eof {
                        match source.try_next()? {
                            Read::Item(v) => pending_input = Some(decode(v)?),
                            Read::End => eof = true,
                            Read::Pending => {}
                        }
                    }
                    if let Some(value) = pending_input.take() {
                        match codec.try_submit(value) {
                            Ok(()) => {}
                            Err(e) if e.error.kind == ErrorKind::WouldBlock => {
                                pending_input = Some(e.value)
                            }
                            Err(e) => return Err(e.error),
                        }
                    }
                    if eof && pending_input.is_none() && flush.is_none() {
                        flush = Some(codec.begin_flush()?);
                    }
                    codec.pump(8)?;
                    if let Some(output) = codec.next_output() {
                        let (item, config) = encode(output);
                        if let Some(config) = config {
                            *lock(&meta)? = Some(config);
                        }
                        pending_output = Some(item);
                    }
                    if pending_output.is_none()
                        && let Some(token) = flush
                        && codec.flush_complete(token)?
                    {
                        sink.finish()?;
                        return Ok(());
                    }
                    thread::sleep(Duration::from_millis(1));
                }
                Ok(())
            })();
            if let Err(error) = &result {
                let _ = sink.fail(error.clone());
            }
            result
        })?;
        Ok(Self {
            input_kind,
            output_kind,
            input: Mutex::new(input),
            output: Mutex::new(Reader::new(output)),
            metadata,
            worker,
        })
    }
    pub fn write(&self, value: Item) -> Result<bool> {
        self.worker.check()?;
        if value.kind() != self.input_kind {
            return Err(Error::invalid("wrong codec input type"));
        }
        send(&mut *lock(&self.input)?, value)
    }
    pub fn finish(&self) -> Result<()> {
        self.worker.check()?;
        lock(&self.input)?.finish()
    }
    pub fn configuration(&self) -> Result<Configuration> {
        self.worker.check()?;
        lock(&self.metadata)?.clone().ok_or_else(|| {
            Error::new(
                ErrorKind::WouldBlock,
                "encoder configuration is available after its first output",
            )
        })
    }
}

pub struct Writer {
    audio: Mutex<Sender<Arc<EncodedChunk>>>,
    video: Mutex<Sender<Arc<EncodedChunk>>>,
    has_audio: bool,
    has_video: bool,
    complete: Arc<AtomicBool>,
    pub worker: Worker,
}
impl Writer {
    pub fn new(
        path: std::path::PathBuf,
        config: xavi_core::mux::Mp4Config,
        limits: Limits,
    ) -> Result<Self> {
        let has_audio = config.audio.is_some();
        let has_video = config.video.is_some();
        let (audio, mut a) = stream::channel::<Arc<EncodedChunk>>(limits)?;
        let (video, mut v) = stream::channel::<Arc<EncodedChunk>>(limits)?;
        let complete = Arc::new(AtomicBool::new(false));
        let done = complete.clone();
        let worker = Worker::start("xavi-mux", move |cancel, initialized| {
            let writer =
                crate::mux::Mp4Writer::create(path, config, limits.max_bytes.min(64 * 1024 * 1024));
            let _ = initialized.send(writer.as_ref().map(|_| ()).map_err(Clone::clone));
            let mut writer = writer?;
            let mut pending = [None, None];
            let mut eof = [!has_audio, !has_video];
            let mut ended = eof;
            while !cancel.load(Ordering::Acquire) {
                for (index, source) in [&mut a, &mut v].into_iter().enumerate() {
                    if pending[index].is_none() && !eof[index] {
                        match source.try_next()? {
                            Read::Item(value) => pending[index] = Some(value),
                            Read::End => eof[index] = true,
                            Read::Pending => {}
                        }
                    }
                }
                let order = if pending[1]
                    .as_ref()
                    .zip(pending[0].as_ref())
                    .is_some_and(|(v, a)| v.timestamp() < a.timestamp())
                {
                    [1, 0]
                } else {
                    [0, 1]
                };
                for index in order {
                    let track = if index == 0 {
                        xavi_core::mux::Track::Audio
                    } else {
                        xavi_core::mux::Track::Video
                    };
                    if let Some(value) = &pending[index]
                        && writer.write(track, value)?
                    {
                        pending[index] = None;
                    }
                    if eof[index] && pending[index].is_none() && !ended[index] {
                        writer.end_track(track)?;
                        ended[index] = true;
                    }
                }
                if ended == [true, true] && writer.finish()? {
                    done.store(true, Ordering::Release);
                    return Ok(());
                }
                thread::sleep(Duration::from_millis(1));
            }
            Ok(())
        })?;
        Ok(Self {
            audio: Mutex::new(audio),
            video: Mutex::new(video),
            has_audio,
            has_video,
            complete,
            worker,
        })
    }
    pub fn write(&self, item: Item) -> Result<bool> {
        self.worker.check()?;
        match item {
            Item::AudioChunk(value) if self.has_audio => send(&mut *lock(&self.audio)?, value),
            Item::VideoChunk(value) if self.has_video => send(&mut *lock(&self.video)?, value),
            _ => Err(Error::invalid("writer has no matching track")),
        }
    }
    pub fn end_audio(&self) -> Result<()> {
        self.worker.check()?;
        lock(&self.audio)?.finish()
    }
    pub fn end_video(&self) -> Result<()> {
        self.worker.check()?;
        lock(&self.video)?.finish()
    }
    pub fn finish(&self) -> Result<()> {
        self.end_audio()?;
        self.end_video()
    }
    pub fn finished(&self) -> Result<bool> {
        self.worker.check()?;
        Ok(self.complete.load(Ordering::Acquire))
    }
}
