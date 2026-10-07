//! Runtime-neutral codec scheduling. A host drives `pump` on its codec worker
//! and delivers `next_output` on its own terms; no VM callback runs here.
//!
//! Input requests and output queues have item and byte limits. At most one
//! additional output, itself bounded by the output byte limit, can be held
//! while waiting for queue space. Codec-private reference frames and one input
//! being processed are additional storage, bounded by the engine configuration.
//! A full output queue stops input consumption. Nothing silently drops frames.
//!
//! Flush is a barrier: stop submitting, pump and consume output until its token
//! completes. Completion means all preceding outputs were taken by the caller;
//! runtime adapters must additionally wait for their callback delivery. A drain
//! reopens the engine so encoding/decoding can continue after flush. Reset and
//! close invalidate outstanding flush tokens and discard pending media.

use std::collections::VecDeque;
use std::sync::Arc;

use crate::stream::{Limits, Payload, SendError};
use crate::{AudioData, EncodedChunk, Error, ErrorKind, Result, VideoColorSpace, VideoFrame};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CodecState {
    Unconfigured,
    Configured,
    Closed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioEncoderConfig {
    pub codec: String,
    pub sample_rate: u32,
    pub channels: u32,
    pub bitrate: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioDecoderConfig {
    pub codec: String,
    pub sample_rate: u32,
    pub channels: u32,
    pub description: Arc<[u8]>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct VideoEncoderConfig {
    pub codec: String,
    pub width: u32,
    pub height: u32,
    pub bitrate: u64,
    pub framerate: f64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VideoDecoderConfig {
    pub codec: String,
    pub coded_width: Option<u32>,
    pub coded_height: Option<u32>,
    pub description: Arc<[u8]>,
    pub color_space: VideoColorSpace,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Support<C> {
    pub supported: bool,
    pub config: C,
}

#[derive(Clone, Debug)]
pub struct VideoEncodeInput {
    pub frame: Arc<VideoFrame>,
    pub key_frame: bool,
}
impl Payload for VideoEncodeInput {
    fn payload_bytes(&self) -> usize {
        self.frame.bytes().len()
    }
}

/// Encoder metadata accompanies the first chunk of each configured segment.
/// Feed it to the decoder before the associated chunk, including after flush.
#[derive(Clone, Debug)]
pub struct EncodedOutput<C> {
    pub chunk: Arc<EncodedChunk>,
    pub decoder_config: Option<C>,
}
impl<C: Description> Payload for EncodedOutput<C> {
    fn payload_bytes(&self) -> usize {
        self.chunk.bytes().len().saturating_add(
            self.decoder_config
                .as_ref()
                .map_or(0, Description::description_bytes),
        )
    }
}
pub trait Description {
    fn description_bytes(&self) -> usize;
}
impl Description for AudioDecoderConfig {
    fn description_bytes(&self) -> usize {
        self.description.len()
    }
}
impl Description for VideoDecoderConfig {
    fn description_bytes(&self) -> usize {
        self.description.len()
    }
}

pub type AudioInput = Arc<AudioData>;
pub type ChunkInput = Arc<EncodedChunk>;

pub enum Receive<T> {
    Output(T),
    Pending,
    End,
}

/// Implementations own native contexts and all media they retain. `send(false)`
/// consumes nothing; the host can retry when the platform signals readiness.
/// `validate` is side-effect free. Invalid input is rejected before entering the
/// queued codec protocol.
pub trait Engine: Sized {
    type Config: Clone;
    type Input: Payload;
    type Output: Payload;
    fn open(config: &Self::Config) -> Result<Self>;
    fn validate(&self, input: &Self::Input) -> Result<()>;
    fn send(&mut self, input: &Self::Input) -> Result<bool>;
    fn receive(&mut self) -> Result<Receive<Self::Output>>;
    /// Progress toward native EOS. False permits another receive/drain cycle.
    fn drain(&mut self) -> Result<bool>;
}

/// Checks an actual codec open, without retaining a reservation or context.
pub fn support<E: Engine>(config: E::Config) -> Result<Support<E::Config>> {
    match E::open(&config) {
        Ok(_) => Ok(Support {
            supported: true,
            config,
        }),
        Err(error) if error.kind == ErrorKind::NotSupported => Ok(Support {
            supported: false,
            config,
        }),
        Err(error) => Err(error),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// A barrier token, valid only for the codec instance that issued it.
pub struct FlushToken {
    epoch: u64,
    serial: u64,
}

pub struct Codec<E: Engine> {
    state: CodecState,
    engine: Option<E>,
    config: Option<E::Config>,
    input_limits: Limits,
    output_limits: Limits,
    inputs: VecDeque<E::Input>,
    input_bytes: usize,
    outputs: VecDeque<E::Output>,
    output_bytes: usize,
    pending_output: Option<E::Output>,
    epoch: u64,
    serial: u64,
    completed: u64,
    flushing: bool,
    draining: bool,
    drained: bool,
    dirty: bool,
}

impl<E: Engine> Codec<E> {
    pub fn new(input: Limits, output: Limits) -> Result<Self> {
        if input.max_items == 0
            || input.max_bytes == 0
            || output.max_items == 0
            || output.max_bytes == 0
        {
            return Err(Error::invalid("codec queue limits must be positive"));
        }
        Ok(Self {
            state: CodecState::Unconfigured,
            engine: None,
            config: None,
            input_limits: input,
            output_limits: output,
            inputs: VecDeque::new(),
            input_bytes: 0,
            outputs: VecDeque::new(),
            output_bytes: 0,
            pending_output: None,
            epoch: 0,
            serial: 0,
            completed: 0,
            flushing: false,
            draining: false,
            drained: false,
            dirty: false,
        })
    }
    pub fn state(&self) -> CodecState {
        self.state
    }
    pub fn queue_size(&self) -> usize {
        self.inputs.len()
    }
    pub fn output_queue_size(&self) -> usize {
        self.outputs.len()
    }

    /// Configuration is synchronous and requires an idle queue. A failed open
    /// leaves the previous configuration intact. Flush before reconfiguration.
    pub fn configure(&mut self, config: E::Config) -> Result<()> {
        self.not_closed()?;
        if self.dirty
            || self.flushing
            || !self.inputs.is_empty()
            || !self.outputs.is_empty()
            || self.pending_output.is_some()
        {
            return Err(blocked("flush and consume output before configuring"));
        }
        let engine = E::open(&config)?;
        self.invalidate()?;
        self.engine = Some(engine);
        self.config = Some(config);
        self.state = CodecState::Configured;
        Ok(())
    }

    pub fn try_submit(&mut self, input: E::Input) -> std::result::Result<(), SendError<E::Input>> {
        let validate = || -> Result<()> {
            self.configured()?;
            if self.flushing {
                return Err(blocked("codec is flushing"));
            }
            self.engine.as_ref().unwrap().validate(&input)?;
            let bytes = input.payload_bytes();
            if bytes > self.input_limits.max_bytes {
                return Err(Error::invalid("codec input exceeds the byte limit"));
            }
            if self.inputs.len() >= self.input_limits.max_items
                || bytes > self.input_limits.max_bytes - self.input_bytes
            {
                return Err(blocked("codec input queue is full"));
            }
            Ok(())
        };
        if let Err(error) = validate() {
            return Err(SendError {
                error,
                value: input,
            });
        }
        if self.inputs.try_reserve(1).is_err() {
            return Err(SendError {
                error: Error::exhausted(),
                value: input,
            });
        }
        self.input_bytes += input.payload_bytes();
        self.inputs.push_back(input);
        Ok(())
    }

    pub fn begin_flush(&mut self) -> Result<FlushToken> {
        self.configured()?;
        if self.flushing {
            return Err(blocked("a flush is already pending"));
        }
        self.serial = self.serial.checked_add(1).ok_or_else(Error::exhausted)?;
        self.flushing = true;
        Ok(FlushToken {
            epoch: self.epoch,
            serial: self.serial,
        })
    }
    pub fn flush_complete(&self, token: FlushToken) -> Result<bool> {
        if token.epoch != self.epoch {
            return Err(Error::new(
                ErrorKind::Cancelled,
                "codec flush was cancelled",
            ));
        }
        Ok(token.serial <= self.completed)
    }

    /// Runs at most `steps` engine cycles. Native calls themselves may take time;
    /// hosts should run this on a codec worker, never their event-loop thread.
    pub fn pump(&mut self, steps: usize) -> Result<usize> {
        self.configured()?;
        let result = self.pump_inner(steps);
        if result.is_err() {
            self.close();
        }
        result
    }
    fn pump_inner(&mut self, steps: usize) -> Result<usize> {
        let mut progressed = 0;
        for _ in 0..steps {
            if let Some(value) = self.pending_output.take() {
                let bytes = value.payload_bytes();
                if bytes > self.output_limits.max_bytes {
                    return Err(Error::new(
                        ErrorKind::ResourceExhausted,
                        "codec output exceeds the byte limit",
                    ));
                }
                if self.outputs.len() >= self.output_limits.max_items
                    || bytes > self.output_limits.max_bytes - self.output_bytes
                {
                    self.pending_output = Some(value);
                    break;
                }
                self.outputs
                    .try_reserve(1)
                    .map_err(|_| Error::exhausted())?;
                self.output_bytes += bytes;
                self.outputs.push_back(value);
                progressed += 1;
            }
            if self.drained {
                if !self.outputs.is_empty() {
                    break;
                }
                self.engine = Some(E::open(self.config.as_ref().unwrap())?);
                self.flushing = false;
                self.draining = false;
                self.drained = false;
                self.completed = self.serial;
                self.dirty = false;
                progressed += 1;
                break;
            }
            if self.outputs.len() >= self.output_limits.max_items
                || self.output_bytes >= self.output_limits.max_bytes
            {
                break;
            }
            let engine = self.engine.as_mut().unwrap();
            match engine.receive()? {
                Receive::Output(value) => {
                    if value.payload_bytes() > self.output_limits.max_bytes {
                        return Err(Error::new(
                            ErrorKind::ResourceExhausted,
                            "codec output exceeds the byte limit",
                        ));
                    }
                    self.pending_output = Some(value);
                    progressed += 1;
                }
                Receive::End => {
                    if !self.draining {
                        return Err(Error::new(
                            ErrorKind::InvalidState,
                            "codec reached EOF outside a flush",
                        ));
                    }
                    self.drained = true;
                    progressed += 1;
                }
                Receive::Pending => {
                    if let Some(input) = self.inputs.front() {
                        if !engine.send(input)? {
                            break;
                        }
                        self.input_bytes -= self.inputs.pop_front().unwrap().payload_bytes();
                        self.dirty = true;
                        progressed += 1;
                    } else if self.flushing && !self.draining {
                        if !engine.drain()? {
                            break;
                        }
                        self.draining = true;
                        progressed += 1;
                    } else {
                        break;
                    }
                }
            }
        }
        Ok(progressed)
    }
    pub fn next_output(&mut self) -> Option<E::Output> {
        let value = self.outputs.pop_front()?;
        self.output_bytes -= value.payload_bytes();
        Some(value)
    }
    pub fn reset(&mut self) -> Result<()> {
        self.not_closed()?;
        self.invalidate()?;
        self.clear();
        self.state = CodecState::Unconfigured;
        Ok(())
    }
    /// Terminal and idempotent; native contexts and queued Arcs are released.
    pub fn close(&mut self) {
        let _ = self.invalidate();
        self.clear();
        self.state = CodecState::Closed;
    }
    fn clear(&mut self) {
        self.engine = None;
        self.config = None;
        self.inputs.clear();
        self.input_bytes = 0;
        self.outputs.clear();
        self.output_bytes = 0;
        self.pending_output = None;
        self.flushing = false;
        self.draining = false;
        self.drained = false;
        self.dirty = false;
    }
    fn invalidate(&mut self) -> Result<()> {
        self.epoch = self.epoch.checked_add(1).ok_or_else(Error::exhausted)?;
        Ok(())
    }
    fn not_closed(&self) -> Result<()> {
        if self.state == CodecState::Closed {
            Err(Error::new(ErrorKind::InvalidState, "codec is closed"))
        } else {
            Ok(())
        }
    }
    fn configured(&self) -> Result<()> {
        if self.state != CodecState::Configured {
            Err(Error::new(
                ErrorKind::InvalidState,
                "codec is not configured",
            ))
        } else {
            Ok(())
        }
    }
}
fn blocked(message: &str) -> Error {
    Error::new(ErrorKind::WouldBlock, message)
}
