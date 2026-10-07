// The implemented native CPU profile of media.idl, consumed by x-idl.
// create() supplies constructors. Counts/byte sizes use i64 carriers so the
// full u32 domain can be checked before conversion; durations are limited to
// nonnegative i64 microseconds. Unknown duration/color metadata stays explicit.
// Empty layout sequences mean the default layout (x-idl collapses omission
// and empty sequences). PixelRect and ColorSpace are immutable value enums;
// PlaneLayouts is the native collection returned by VideoFrame.copyTo.
// Encoded chunk kind() exposes the IDL's `type` attribute.
// All metadata access after close raises; close itself is idempotent.
// Every resource, including chunks and PlaneLayouts, must be closed. Generated
// wrappers do not release media handles on collection. Context teardown also
// releases all resources. Handles must stay within their owning context.

#[idl("AudioSampleFormat")]
enum AudioSampleFormat {}
#[idl("VideoPixelFormat")]
enum VideoPixelFormat {}
#[idl("EncodedChunkType")]
enum EncodedChunkType {}
#[idl("VideoColorPrimaries")]
enum VideoColorPrimaries {}
#[idl("VideoTransferCharacteristics")]
enum VideoTransferCharacteristics {}
#[idl("VideoMatrixCoefficients")]
enum VideoMatrixCoefficients {}

#[idl("AudioDataInit")]
struct AudioDataInit {
    numberOfFrames: i64,
    numberOfChannels: i64,
    data: Buffer,
}
#[idl("AudioDataCopyToOptions")]
struct AudioDataCopyToOptions {
    planeIndex: i64,
    frameOffset: Option<i64>,
    frameCount: Option<i64>,
}
#[idl("DOMRectInit")]
struct DOMRectInit {
    x: Option<f64>,
    y: Option<f64>,
    width: Option<f64>,
    height: Option<f64>,
}
#[idl("PlaneLayout")]
struct PlaneLayout {
    offset: i64,
    stride: i64,
}
#[idl("VideoColorSpaceInit")]
struct VideoColorSpaceInit {}
#[idl("VideoFrameBufferInit")]
struct VideoFrameBufferInit {
    codedWidth: i64,
    codedHeight: i64,
    displayWidth: Option<i64>,
    displayHeight: Option<i64>,
}
#[idl("VideoFrameCopyToOptions")]
struct VideoFrameCopyToOptions {}
#[idl("EncodedAudioChunkInit")]
struct EncodedAudioChunkInit {
    data: Buffer,
}
#[idl("EncodedVideoChunkInit")]
struct EncodedVideoChunkInit {
    data: Buffer,
}

enum OptionalDuration {
    Unknown,
    Value { microseconds: i64 },
}
enum OptionalPrimaries {
    Unknown,
    Value { value: Enum<VideoColorPrimaries> },
}
enum OptionalTransfer {
    Unknown,
    Value {
        value: Enum<VideoTransferCharacteristics>,
    },
}
enum OptionalMatrix {
    Unknown,
    Value {
        value: Enum<VideoMatrixCoefficients>,
    },
}
enum OptionalBool {
    Unknown,
    Value { value: bool },
}
enum ColorSpace {
    Value {
        primaries: OptionalPrimaries,
        transfer: OptionalTransfer,
        matrix: OptionalMatrix,
        fullRange: OptionalBool,
    },
}
enum PixelRect {
    Value {
        x: i64,
        y: i64,
        width: i64,
        height: i64,
    },
}

trait AudioData {
    #[native(audio_slice)]
    fn slice(this: &AudioData, offset: i64, count: i64, timestamp: i64) -> Box<AudioData>;
    #[native(audio_retime)]
    fn retime(this: &AudioData, timestamp: i64) -> Box<AudioData>;
    #[native(audio_gain)]
    fn gain(this: &AudioData, gain: f64) -> Box<AudioData>;
    #[native(audio_mix)]
    fn mix(this: &AudioData, other: &AudioData, gain: f64) -> Box<AudioData>;
    #[native(audio_create)]
    fn create(init: &AudioDataInit) -> Box<AudioData>;
    #[native(audio_format)]
    fn format(this: &AudioData) -> Enum<AudioSampleFormat>;
    #[native(audio_sample_rate)]
    fn sampleRate(this: &AudioData) -> f32;
    #[native(audio_frames)]
    fn numberOfFrames(this: &AudioData) -> i64;
    #[native(audio_channels)]
    fn numberOfChannels(this: &AudioData) -> i64;
    #[native(audio_timestamp)]
    fn timestamp(this: &AudioData) -> i64;
    #[native(audio_duration)]
    fn duration(this: &AudioData) -> i64;
    #[native(audio_allocation_size)]
    fn allocationSize(this: &AudioData, options: &AudioDataCopyToOptions) -> i64;
    #[native(audio_copy_to)]
    fn copyTo(this: &AudioData, destination: BufferMut, options: &AudioDataCopyToOptions);
    #[native(audio_clone)]
    fn clone(this: &AudioData) -> Box<AudioData>;
    #[native(audio_close)]
    fn close(this: &AudioData);
}

trait AudioEqualizer {
    #[native(equalizer_create)]
    fn create(bands: i32) -> Box<AudioEqualizer>;
    #[native(equalizer_band_count)]
    fn bandCount(this: &AudioEqualizer) -> i32;
    #[native(equalizer_set_band)]
    fn setBand(this: &AudioEqualizer, index: i32, frequency: f64, gainDb: f64, q: f64);
    #[native(equalizer_disable_band)]
    fn disableBand(this: &AudioEqualizer, index: i32);
    #[native(equalizer_band_frequency)]
    fn bandFrequency(this: &AudioEqualizer, index: i32) -> f64;
    #[native(equalizer_band_gain)]
    fn bandGain(this: &AudioEqualizer, index: i32) -> f64;
    #[native(equalizer_band_q)]
    fn bandQ(this: &AudioEqualizer, index: i32) -> f64;
    #[native(equalizer_band_enabled)]
    fn bandEnabled(this: &AudioEqualizer, index: i32) -> bool;
    #[native(equalizer_set_preamp)]
    fn setPreamp(this: &AudioEqualizer, gainDb: f64);
    #[native(equalizer_preamp)]
    fn preamp(this: &AudioEqualizer) -> f64;
    #[native(equalizer_set_bypass)]
    fn setBypass(this: &AudioEqualizer, bypass: bool);
    #[native(equalizer_bypassed)]
    fn bypassed(this: &AudioEqualizer) -> bool;
    #[native(equalizer_process)]
    fn process(this: &AudioEqualizer, data: &AudioData) -> Box<AudioData>;
    #[native(equalizer_reset)]
    fn reset(this: &AudioEqualizer);
    #[native(equalizer_close)]
    fn close(this: &AudioEqualizer);
}

trait VideoFrame {
    #[native(video_retime)]
    fn retime(this: &VideoFrame, timestamp: i64, duration: i64) -> Box<VideoFrame>;
    #[native(video_crop)]
    fn crop(this: &VideoFrame, x: i64, y: i64, width: i64, height: i64) -> Box<VideoFrame>;
    #[native(video_resize)]
    fn resize(this: &VideoFrame, width: i64, height: i64) -> Box<VideoFrame>;
    #[native(video_blend)]
    fn blend(this: &VideoFrame, other: &VideoFrame, opacity: f64) -> Box<VideoFrame>;
    #[native(video_create)]
    fn create(data: Buffer, init: &VideoFrameBufferInit) -> Box<VideoFrame>;
    #[native(video_format)]
    fn format(this: &VideoFrame) -> Enum<VideoPixelFormat>;
    #[native(video_coded_width)]
    fn codedWidth(this: &VideoFrame) -> i64;
    #[native(video_coded_height)]
    fn codedHeight(this: &VideoFrame) -> i64;
    #[native(video_display_width)]
    fn displayWidth(this: &VideoFrame) -> i64;
    #[native(video_display_height)]
    fn displayHeight(this: &VideoFrame) -> i64;
    #[native(video_visible_rect)]
    fn visibleRect(this: &VideoFrame) -> PixelRect;
    #[native(video_color_space)]
    fn colorSpace(this: &VideoFrame) -> ColorSpace;
    #[native(video_timestamp)]
    fn timestamp(this: &VideoFrame) -> i64;
    #[native(video_duration)]
    fn duration(this: &VideoFrame) -> OptionalDuration;
    #[native(video_allocation_size)]
    fn allocationSize(this: &VideoFrame, options: &VideoFrameCopyToOptions) -> i64;
    #[native(video_copy_to)]
    fn copyTo(
        this: &VideoFrame,
        destination: BufferMut,
        options: &VideoFrameCopyToOptions,
    ) -> Future<PlaneLayouts>;
    #[native(video_clone)]
    fn clone(this: &VideoFrame) -> Box<VideoFrame>;
    #[native(video_close)]
    fn close(this: &VideoFrame);
}

/// Destination layouts, in pixel-format plane order. Close after reading.
trait PlaneLayouts {
    #[native(layouts_count)]
    fn count(this: &PlaneLayouts) -> i32;
    #[native(layouts_offset)]
    fn offset(this: &PlaneLayouts, index: i32) -> i64;
    #[native(layouts_stride)]
    fn stride(this: &PlaneLayouts, index: i32) -> i64;
    #[native(layouts_close)]
    fn close(this: &PlaneLayouts);
}

trait EncodedAudioChunk {
    #[native(audio_chunk_create)]
    fn create(init: &EncodedAudioChunkInit) -> Box<EncodedAudioChunk>;
    #[native(audio_chunk_type)]
    fn kind(this: &EncodedAudioChunk) -> Enum<EncodedChunkType>;
    #[native(audio_chunk_timestamp)]
    fn timestamp(this: &EncodedAudioChunk) -> i64;
    #[native(audio_chunk_duration)]
    fn duration(this: &EncodedAudioChunk) -> OptionalDuration;
    #[native(audio_chunk_byte_length)]
    fn byteLength(this: &EncodedAudioChunk) -> i64;
    #[native(audio_chunk_copy_to)]
    fn copyTo(this: &EncodedAudioChunk, destination: BufferMut);
    #[native(audio_chunk_close)]
    fn close(this: &EncodedAudioChunk);
}

trait EncodedVideoChunk {
    #[native(video_chunk_create)]
    fn create(init: &EncodedVideoChunkInit) -> Box<EncodedVideoChunk>;
    #[native(video_chunk_type)]
    fn kind(this: &EncodedVideoChunk) -> Enum<EncodedChunkType>;
    #[native(video_chunk_timestamp)]
    fn timestamp(this: &EncodedVideoChunk) -> i64;
    #[native(video_chunk_duration)]
    fn duration(this: &EncodedVideoChunk) -> OptionalDuration;
    #[native(video_chunk_byte_length)]
    fn byteLength(this: &EncodedVideoChunk) -> i64;
    #[native(video_chunk_copy_to)]
    fn copyTo(this: &EncodedVideoChunk, destination: BufferMut);
    #[native(video_chunk_close)]
    fn close(this: &EncodedVideoChunk);
}

#[idl("PlaybackState")]
enum PlaybackState {}

/// A native file player with audio output and one polled video frame.
/// Drive on the main event-loop thread. Close each frame after uploading it.
trait MediaPlayer {
    #[native(player_open)]
    fn open(path: Text) -> Box<MediaPlayer>;
    #[native(player_state)]
    fn state(this: &MediaPlayer) -> Enum<PlaybackState>;
    #[native(player_position)]
    fn position(this: &MediaPlayer) -> f64;
    #[native(player_duration)]
    fn duration(this: &MediaPlayer) -> f64;
    #[native(player_volume)]
    fn volume(this: &MediaPlayer) -> f64;
    #[native(player_play)]
    fn play(this: &MediaPlayer);
    #[native(player_pause)]
    fn pause(this: &MediaPlayer);
    #[native(player_seek)]
    fn seek(this: &MediaPlayer, seconds: f64);
    #[native(player_set_volume)]
    fn setVolume(this: &MediaPlayer, volume: f64);
    #[native(player_set_equalizer)]
    fn setEqualizer(this: &MediaPlayer, equalizer: &AudioEqualizer);
    #[native(player_clear_equalizer)]
    fn clearEqualizer(this: &MediaPlayer);
    #[native(player_poll_frame)]
    fn pollFrame(this: &MediaPlayer) -> bool;
    #[native(player_take_frame)]
    fn takeFrame(this: &MediaPlayer) -> Box<VideoFrame>;
    #[native(player_close)]
    fn close(this: &MediaPlayer);
}

#[idl("StreamReadStatus")]
enum StreamReadStatus {}
#[idl("MediaPayloadKind")]
enum MediaPayloadKind {}

trait CodecConfiguration {
    #[native(configuration_audio)]
    fn audio(codec: Text, sampleRate: i64, channels: i64, description: Buffer) -> Box<CodecConfiguration>;
    #[native(configuration_video)]
    fn video(codec: Text, width: i64, height: i64, description: Buffer) -> Box<CodecConfiguration>;
    #[native(configuration_codec)]
    fn codec(this: &CodecConfiguration) -> Text;
    #[native(configuration_sample_rate)]
    fn sampleRate(this: &CodecConfiguration) -> i64;
    #[native(configuration_channels)]
    fn numberOfChannels(this: &CodecConfiguration) -> i64;
    #[native(configuration_width)]
    fn codedWidth(this: &CodecConfiguration) -> i64;
    #[native(configuration_height)]
    fn codedHeight(this: &CodecConfiguration) -> i64;
    #[native(configuration_description_size)]
    fn descriptionSize(this: &CodecConfiguration) -> i64;
    #[native(configuration_copy_description)]
    fn copyDescription(this: &CodecConfiguration, destination: BufferMut);
    #[native(configuration_close)]
    fn close(this: &CodecConfiguration);
}

trait MediaQueue {
    #[native(queue_create)]
    fn create(kind: Enum<MediaPayloadKind>, maxItems: i32, maxBytes: i64) -> Box<MediaQueue>;
    #[native(queue_poll)]
    fn poll(this: &MediaQueue) -> Enum<StreamReadStatus>;
    #[native(queue_finish)]
    fn finish(this: &MediaQueue);
    #[native(queue_close)]
    fn close(this: &MediaQueue);
    #[native(queue_write_audio)]
    fn writeAudio(this: &MediaQueue, value: &AudioData) -> bool;
    #[native(queue_read_audio)]
    fn readAudio(this: &MediaQueue) -> Box<AudioData>;
    #[native(queue_write_video)]
    fn writeVideo(this: &MediaQueue, value: &VideoFrame) -> bool;
    #[native(queue_read_video)]
    fn readVideo(this: &MediaQueue) -> Box<VideoFrame>;
    #[native(queue_write_audio_chunk)]
    fn writeAudioChunk(this: &MediaQueue, value: &EncodedAudioChunk) -> bool;
    #[native(queue_read_audio_chunk)]
    fn readAudioChunk(this: &MediaQueue) -> Box<EncodedAudioChunk>;
    #[native(queue_write_video_chunk)]
    fn writeVideoChunk(this: &MediaQueue, value: &EncodedVideoChunk) -> bool;
    #[native(queue_read_video_chunk)]
    fn readVideoChunk(this: &MediaQueue) -> Box<EncodedVideoChunk>;
    #[native(queue_write_bytes)]
    fn writeBytes(this: &MediaQueue, value: Buffer) -> bool;
    #[native(queue_byte_length)]
    fn byteLength(this: &MediaQueue) -> i64;
    #[native(queue_read_bytes)]
    fn readBytes(this: &MediaQueue, destination: BufferMut) -> i64;
}

trait MediaDemuxer {
    #[native(demux_open)]
    fn open(path: Text, maxItems: i32, maxBytes: i64) -> Box<MediaDemuxer>;
    #[native(demux_has_audio)]
    fn hasAudio(this: &MediaDemuxer) -> bool;
    #[native(demux_has_video)]
    fn hasVideo(this: &MediaDemuxer) -> bool;
    #[native(demux_duration)]
    fn duration(this: &MediaDemuxer) -> f64;
    #[native(demux_close)]
    fn close(this: &MediaDemuxer);
    #[native(demux_audio_configuration)]
    fn getAudioConfiguration(this: &MediaDemuxer) -> Box<CodecConfiguration>;
    #[native(demux_audio_status)]
    fn audioStatus(this: &MediaDemuxer) -> Enum<StreamReadStatus>;
    #[native(demux_read_audio)]
    fn readAudioChunk(this: &MediaDemuxer) -> Box<EncodedAudioChunk>;
    #[native(demux_video_configuration)]
    fn getVideoConfiguration(this: &MediaDemuxer) -> Box<CodecConfiguration>;
    #[native(demux_video_status)]
    fn videoStatus(this: &MediaDemuxer) -> Enum<StreamReadStatus>;
    #[native(demux_read_video)]
    fn readVideoChunk(this: &MediaDemuxer) -> Box<EncodedVideoChunk>;
}

trait MediaMuxer {
    #[native(mux_audio)]
    fn audio(path: Text, audio: &CodecConfiguration, origin: i64, maxItems: i32, maxBytes: i64) -> Box<MediaMuxer>;
    #[native(mux_video)]
    fn video(path: Text, video: &CodecConfiguration, origin: i64, defaultDuration: i64, maxItems: i32, maxBytes: i64) -> Box<MediaMuxer>;
    #[native(mux_audio_video)]
    fn audioVideo(path: Text, audio: &CodecConfiguration, video: &CodecConfiguration, origin: i64, defaultDuration: i64, maxItems: i32, maxBytes: i64) -> Box<MediaMuxer>;
    #[native(mux_finish)]
    fn finish(this: &MediaMuxer);
    #[native(mux_finished)]
    fn finished(this: &MediaMuxer) -> bool;
    #[native(mux_close)]
    fn close(this: &MediaMuxer);
    #[native(mux_write_audio)]
    fn writeAudioChunk(this: &MediaMuxer, value: &EncodedAudioChunk) -> bool;
    #[native(mux_end_audio)]
    fn endAudioTrack(this: &MediaMuxer);
    #[native(mux_write_video)]
    fn writeVideoChunk(this: &MediaMuxer, value: &EncodedVideoChunk) -> bool;
    #[native(mux_end_video)]
    fn endVideoTrack(this: &MediaMuxer);
}

trait AudioEncoder {
    #[native(audio_encoder_create)]
    fn create(codec: Text, sampleRate: i64, channels: i64, bitrate: i64, maxItems: i32, maxBytes: i64) -> Box<AudioEncoder>;
    #[native(audio_encoder_write)]
    fn tryEncode(this: &AudioEncoder, value: &AudioData) -> bool;
    #[native(audio_encoder_poll)]
    fn poll(this: &AudioEncoder) -> Enum<StreamReadStatus>;
    #[native(audio_encoder_read)]
    fn read(this: &AudioEncoder) -> Box<EncodedAudioChunk>;
    #[native(audio_encoder_finish)]
    fn finish(this: &AudioEncoder);
    #[native(audio_encoder_close)]
    fn close(this: &AudioEncoder);
    #[native(audio_encoder_configuration)]
    fn getConfiguration(this: &AudioEncoder) -> Box<CodecConfiguration>;
}

trait VideoEncoder {
    #[native(video_encoder_create)]
    fn create(codec: Text, width: i64, height: i64, bitrate: i64, framerate: f64, maxItems: i32, maxBytes: i64) -> Box<VideoEncoder>;
    #[native(video_encoder_write)]
    fn tryEncode(this: &VideoEncoder, value: &VideoFrame) -> bool;
    #[native(video_encoder_poll)]
    fn poll(this: &VideoEncoder) -> Enum<StreamReadStatus>;
    #[native(video_encoder_read)]
    fn read(this: &VideoEncoder) -> Box<EncodedVideoChunk>;
    #[native(video_encoder_finish)]
    fn finish(this: &VideoEncoder);
    #[native(video_encoder_close)]
    fn close(this: &VideoEncoder);
    #[native(video_encoder_configuration)]
    fn getConfiguration(this: &VideoEncoder) -> Box<CodecConfiguration>;
}

trait AudioDecoder {
    #[native(audio_decoder_create)]
    fn create(maxItems: i32, maxBytes: i64, configuration: &CodecConfiguration) -> Box<AudioDecoder>;
    #[native(audio_decoder_write)]
    fn tryDecode(this: &AudioDecoder, value: &EncodedAudioChunk) -> bool;
    #[native(audio_decoder_poll)]
    fn poll(this: &AudioDecoder) -> Enum<StreamReadStatus>;
    #[native(audio_decoder_read)]
    fn read(this: &AudioDecoder) -> Box<AudioData>;
    #[native(audio_decoder_finish)]
    fn finish(this: &AudioDecoder);
    #[native(audio_decoder_close)]
    fn close(this: &AudioDecoder);
}

trait VideoDecoder {
    #[native(video_decoder_create)]
    fn create(maxItems: i32, maxBytes: i64, configuration: &CodecConfiguration) -> Box<VideoDecoder>;
    #[native(video_decoder_write)]
    fn tryDecode(this: &VideoDecoder, value: &EncodedVideoChunk) -> bool;
    #[native(video_decoder_poll)]
    fn poll(this: &VideoDecoder) -> Enum<StreamReadStatus>;
    #[native(video_decoder_read)]
    fn read(this: &VideoDecoder) -> Box<VideoFrame>;
    #[native(video_decoder_finish)]
    fn finish(this: &VideoDecoder);
    #[native(video_decoder_close)]
    fn close(this: &VideoDecoder);
}
