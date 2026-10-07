//! Synchronous in-place PCM MFT inserted before Media Engine's audio renderer.
//! One retained sample provides backpressure; timestamps and attributes survive.
use super::equalizer::{Control, Stream};
use std::mem::ManuallyDrop;
use std::sync::{Mutex, MutexGuard};
use windows::Win32::{
    Foundation::{E_FAIL, E_INVALIDARG, E_NOTIMPL, E_POINTER},
    Media::MediaFoundation::*,
};
use windows::core::{Ref, Result, implement};

struct State {
    input: Option<IMFMediaType>,
    output: Option<IMFMediaType>,
    pending: Option<IMFSample>,
    dsp: Stream,
    draining: bool,
}
#[implement(IMFTransform)]
struct Equalizer(Mutex<State>);

pub(super) fn create(control: Control) -> IMFTransform {
    Equalizer(Mutex::new(State {
        input: None,
        output: None,
        pending: None,
        dsp: Stream::new(control),
        draining: false,
    }))
    .into()
}
fn id(stream: u32) -> Result<()> {
    if stream == 0 {
        Ok(())
    } else {
        Err(MF_E_INVALIDSTREAMNUMBER.into())
    }
}
fn put<T>(ptr: *mut T, value: T) -> Result<()> {
    if ptr.is_null() {
        Err(E_POINTER.into())
    } else {
        unsafe {
            ptr.write(value);
        }
        Ok(())
    }
}
fn format(t: &IMFMediaType) -> Result<(u32, u32, bool)> {
    unsafe {
        let sub = t.GetGUID(&MF_MT_SUBTYPE)?;
        let float = sub == MFAudioFormat_Float;
        let rate = t.GetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND)?;
        let channels = t.GetUINT32(&MF_MT_AUDIO_NUM_CHANNELS)?;
        let unit = if float { 4 } else { 2 };
        if t.GetGUID(&MF_MT_MAJOR_TYPE)? != MFMediaType_Audio
            || (!float && sub != MFAudioFormat_PCM)
            || !(1000..=384000).contains(&rate)
            || !(1..=32).contains(&channels)
            || t.GetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE)? != unit * 8
            || t.GetUINT32(&MF_MT_AUDIO_BLOCK_ALIGNMENT)? != channels * unit
        {
            return Err(MF_E_INVALIDMEDIATYPE.into());
        }
        Ok((rate, channels, float))
    }
}
fn duplicate(t: &IMFMediaType) -> Result<IMFMediaType> {
    unsafe {
        let copy = MFCreateMediaType()?;
        t.CopyAllItems(&copy)?;
        Ok(copy)
    }
}
impl Equalizer_Impl {
    fn state(&self) -> Result<MutexGuard<'_, State>> {
        self.0.lock().map_err(|_| E_FAIL.into())
    }
    fn set_type(&self, input: bool, t: Ref<IMFMediaType>, flags: u32) -> Result<()> {
        if flags & !(MFT_SET_TYPE_TEST_ONLY.0 as u32) != 0 {
            return Err(E_INVALIDARG.into());
        }
        let mut state = self.state()?;
        if state.pending.is_some() {
            return Err(MF_E_TRANSFORM_CANNOT_CHANGE_MEDIATYPE_WHILE_PROCESSING.into());
        }
        if let Some(t) = t.as_ref() {
            let fmt = format(t)?;
            let other = if input { &state.output } else { &state.input };
            if let Some(other) = other
                && format(other)? != fmt
            {
                return Err(MF_E_INVALIDMEDIATYPE.into());
            }
        }
        if flags == 0 {
            let value = t.as_ref().map(duplicate).transpose()?;
            if input {
                state.input = value;
            } else {
                state.output = value;
            }
            state.dsp.processor.reset();
        }
        Ok(())
    }
}
impl IMFTransform_Impl for Equalizer_Impl {
    fn GetStreamLimits(&self, a: *mut u32, b: *mut u32, c: *mut u32, d: *mut u32) -> Result<()> {
        put(a, 1)?;
        put(b, 1)?;
        put(c, 1)?;
        put(d, 1)
    }
    fn GetStreamCount(&self, a: *mut u32, b: *mut u32) -> Result<()> {
        put(a, 1)?;
        put(b, 1)
    }
    fn GetStreamIDs(&self, _: u32, _: *mut u32, _: u32, _: *mut u32) -> Result<()> {
        Err(E_NOTIMPL.into())
    }
    fn GetInputStreamInfo(&self, stream: u32, out: *mut MFT_INPUT_STREAM_INFO) -> Result<()> {
        id(stream)?;
        put(
            out,
            MFT_INPUT_STREAM_INFO {
                dwFlags: (MFT_INPUT_STREAM_WHOLE_SAMPLES.0 | MFT_INPUT_STREAM_PROCESSES_IN_PLACE.0)
                    as u32,
                ..Default::default()
            },
        )
    }
    fn GetOutputStreamInfo(&self, stream: u32) -> Result<MFT_OUTPUT_STREAM_INFO> {
        id(stream)?;
        Ok(MFT_OUTPUT_STREAM_INFO {
            dwFlags: (MFT_OUTPUT_STREAM_WHOLE_SAMPLES.0 | MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0)
                as u32,
            ..Default::default()
        })
    }
    fn GetAttributes(&self) -> Result<IMFAttributes> {
        Err(E_NOTIMPL.into())
    }
    fn GetInputStreamAttributes(&self, _: u32) -> Result<IMFAttributes> {
        Err(E_NOTIMPL.into())
    }
    fn GetOutputStreamAttributes(&self, _: u32) -> Result<IMFAttributes> {
        Err(E_NOTIMPL.into())
    }
    fn DeleteInputStream(&self, _: u32) -> Result<()> {
        Err(E_NOTIMPL.into())
    }
    fn AddInputStreams(&self, _: u32, _: *const u32) -> Result<()> {
        Err(E_NOTIMPL.into())
    }
    fn GetInputAvailableType(&self, stream: u32, index: u32) -> Result<IMFMediaType> {
        id(stream)?;
        let state = self.state()?;
        if let Some(out) = &state.output {
            return if index == 0 {
                duplicate(out)
            } else {
                Err(MF_E_NO_MORE_TYPES.into())
            };
        }
        if index >= 2 {
            return Err(MF_E_NO_MORE_TYPES.into());
        }
        unsafe {
            let t = MFCreateMediaType()?;
            t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
            t.SetGUID(
                &MF_MT_SUBTYPE,
                if index == 0 {
                    &MFAudioFormat_Float
                } else {
                    &MFAudioFormat_PCM
                },
            )?;
            Ok(t)
        }
    }
    fn GetOutputAvailableType(&self, stream: u32, index: u32) -> Result<IMFMediaType> {
        id(stream)?;
        if index != 0 {
            return Err(MF_E_NO_MORE_TYPES.into());
        }
        self.GetInputCurrentType(stream)
    }
    fn SetInputType(&self, stream: u32, t: Ref<IMFMediaType>, flags: u32) -> Result<()> {
        id(stream)?;
        self.set_type(true, t, flags)
    }
    fn SetOutputType(&self, stream: u32, t: Ref<IMFMediaType>, flags: u32) -> Result<()> {
        id(stream)?;
        self.set_type(false, t, flags)
    }
    fn GetInputCurrentType(&self, stream: u32) -> Result<IMFMediaType> {
        id(stream)?;
        duplicate(
            self.state()?
                .input
                .as_ref()
                .ok_or(MF_E_TRANSFORM_TYPE_NOT_SET)?,
        )
    }
    fn GetOutputCurrentType(&self, stream: u32) -> Result<IMFMediaType> {
        id(stream)?;
        duplicate(
            self.state()?
                .output
                .as_ref()
                .ok_or(MF_E_TRANSFORM_TYPE_NOT_SET)?,
        )
    }
    fn GetInputStatus(&self, stream: u32) -> Result<u32> {
        id(stream)?;
        let s = self.state()?;
        Ok(
            if s.input.is_some() && s.output.is_some() && s.pending.is_none() && !s.draining {
                MFT_INPUT_STATUS_ACCEPT_DATA.0 as u32
            } else {
                0
            },
        )
    }
    fn GetOutputStatus(&self) -> Result<u32> {
        Ok(if self.state()?.pending.is_some() {
            MFT_OUTPUT_STATUS_SAMPLE_READY.0 as u32
        } else {
            0
        })
    }
    fn SetOutputBounds(&self, _: i64, _: i64) -> Result<()> {
        Err(E_NOTIMPL.into())
    }
    fn ProcessEvent(&self, _: u32, _: Ref<IMFMediaEvent>) -> Result<()> {
        Err(E_NOTIMPL.into())
    }
    fn ProcessMessage(&self, message: MFT_MESSAGE_TYPE, _: usize) -> Result<()> {
        let mut state = self.state()?;
        match message {
            MFT_MESSAGE_COMMAND_FLUSH
            | MFT_MESSAGE_NOTIFY_START_OF_STREAM
            | MFT_MESSAGE_NOTIFY_END_STREAMING => {
                state.pending = None;
                state.draining = false;
                state.dsp.processor.reset();
            }
            MFT_MESSAGE_COMMAND_DRAIN => state.draining = true,
            MFT_MESSAGE_SET_D3D_MANAGER => return Err(E_NOTIMPL.into()),
            _ => {}
        }
        Ok(())
    }
    fn ProcessInput(&self, stream: u32, sample: Ref<IMFSample>, flags: u32) -> Result<()> {
        id(stream)?;
        if flags != 0 {
            return Err(E_INVALIDARG.into());
        }
        let sample = sample.as_ref().ok_or(E_POINTER)?;
        let mut state = self.state()?;
        if state.pending.is_some() || state.draining {
            return Err(MF_E_NOTACCEPTING.into());
        }
        let (rate, channels, float) =
            format(state.input.as_ref().ok_or(MF_E_TRANSFORM_TYPE_NOT_SET)?)?;
        if state.output.is_none() {
            return Err(MF_E_TRANSFORM_TYPE_NOT_SET.into());
        }
        unsafe {
            let buffer = sample.ConvertToContiguousBuffer()?;
            let mut data = std::ptr::null_mut();
            let mut length = 0;
            buffer.Lock(&mut data, None, Some(&mut length))?;
            let result = if length == 0 {
                Ok(())
            } else if data.is_null() || length as usize > crate::MAX_BYTES {
                Err(E_INVALIDARG.into())
            } else {
                let reset = sample
                    .GetUINT32(&MFSampleExtension_Discontinuity)
                    .unwrap_or(0)
                    != 0;
                state
                    .dsp
                    .interleaved(
                        std::slice::from_raw_parts_mut(data, length as usize),
                        rate as f64,
                        channels as usize,
                        float,
                        reset,
                    )
                    .map_err(|_| windows::core::Error::from(E_FAIL))
            };
            buffer.Unlock()?;
            result?;
        }
        state.pending = Some(sample.clone());
        Ok(())
    }
    fn ProcessOutput(
        &self,
        flags: u32,
        count: u32,
        out: *mut MFT_OUTPUT_DATA_BUFFER,
        status: *mut u32,
    ) -> Result<()> {
        if flags != 0 || count != 1 {
            return Err(E_INVALIDARG.into());
        }
        if out.is_null() || status.is_null() {
            return Err(E_POINTER.into());
        }
        let out = unsafe { &mut *out };
        if out.dwStreamID != 0 || out.pSample.is_some() || out.pEvents.is_some() {
            return Err(E_INVALIDARG.into());
        }
        let mut state = self.state()?;
        let sample = state.pending.take().ok_or(MF_E_TRANSFORM_NEED_MORE_INPUT)?;
        out.pSample = ManuallyDrop::new(Some(sample));
        out.pEvents = ManuallyDrop::new(None);
        out.dwStatus = 0;
        put(status, 0)
    }
}
