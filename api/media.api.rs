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

trait VideoFrame {
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
