#include "native.h"
#include <AudioToolbox/AudioToolbox.h>
#include <CoreMedia/CoreMedia.h>
#include <CoreVideo/CoreVideo.h>
#include <VideoToolbox/VideoToolbox.h>
#include <limits.h>
#include <stdlib.h>
#include <string.h>

#define MAX_BYTES (64u * 1024u * 1024u)
#define NEED_INPUT 1001

struct XaviCodec {
  XaviConfig config;
  AudioConverterRef audio;
  VTCompressionSessionRef encoder;
  VTDecompressionSessionRef decoder;
  CMVideoFormatDescriptionRef video_format;
  XaviOutput *output;
  int32_t callback_error;
  uint8_t *input, *retired_input;
  size_t input_len, input_pos;
  AudioStreamPacketDescription input_packet;
  uint32_t packet_capacity;
  uint64_t output_frames;
  int64_t origin;
  int have_origin, eos, first_output;
};

static int32_t failed(OSStatus status) {
  return status == 0 ? XAVI_OK : XAVI_FAILED;
}
void xavi_output_free(XaviOutput *o) {
  if (o) {
    free(o->data);
    free(o->description);
    free(o);
  }
}
static XaviOutput *output_new(size_t len) {
  if (len > MAX_BYTES)
    return NULL;
  XaviOutput *o = calloc(1, sizeof(*o));
  if (!o)
    return NULL;
  o->data = malloc(len ? len : 1);
  if (!o->data) {
    free(o);
    return NULL;
  }
  o->len = len;
  return o;
}
static int64_t micros(CMTime t) {
  return CMTIME_IS_NUMERIC(t)
             ? CMTimeConvertScale(t, 1000000,
                                  kCMTimeRoundingMethod_RoundTowardZero)
                   .value
             : 0;
}
static OSStatus property(VTSessionRef session, CFStringRef key, int64_t value) {
  CFNumberRef n = CFNumberCreate(NULL, kCFNumberSInt64Type, &value);
  if (!n)
    return -1;
  OSStatus status = VTSessionSetProperty(session, key, n);
  CFRelease(n);
  return status;
}

static int copy_avcc(CMFormatDescriptionRef format, XaviOutput *o) {
  const uint8_t *sps = NULL, *pps = NULL;
  size_t sn = 0, pn = 0, count = 0;
  int header = 0;
  if (CMVideoFormatDescriptionGetH264ParameterSetAtIndex(format, 0, &sps, &sn,
                                                         &count, &header) ||
      count != 2 || header != 4 || sn < 4 || sn > UINT16_MAX)
    return 0;
  if (CMVideoFormatDescriptionGetH264ParameterSetAtIndex(format, 1, &pps, &pn,
                                                         NULL, NULL) ||
      pn > UINT16_MAX)
    return 0;
  o->description_len = 11 + sn + pn;
  o->description = malloc(o->description_len);
  if (!o->description)
    return 0;
  uint8_t *p = o->description;
  *p++ = 1;
  *p++ = sps[1];
  *p++ = sps[2];
  *p++ = sps[3];
  *p++ = 255;
  *p++ = 225;
  *p++ = (uint8_t)(sn >> 8);
  *p++ = (uint8_t)sn;
  memcpy(p, sps, sn);
  p += sn;
  *p++ = 1;
  *p++ = (uint8_t)(pn >> 8);
  *p++ = (uint8_t)pn;
  memcpy(p, pps, pn);
  return 1;
}
static void encoded(void *ref, void *source, OSStatus status,
                    VTEncodeInfoFlags flags, CMSampleBufferRef sample) {
  (void)source;
  (void)flags;
  XaviCodec *c = ref;
  if (status || !sample || c->output) {
    c->callback_error = XAVI_FAILED;
    return;
  }
  CMBlockBufferRef block = CMSampleBufferGetDataBuffer(sample);
  if (!block) {
    c->callback_error = XAVI_FAILED;
    return;
  }
  XaviOutput *o = output_new(CMBlockBufferGetDataLength(block));
  if (!o) {
    c->callback_error = XAVI_RESOURCE;
    return;
  }
  if (CMBlockBufferCopyDataBytes(block, 0, o->len, o->data)) {
    xavi_output_free(o);
    c->callback_error = XAVI_FAILED;
    return;
  }
  o->timestamp = micros(CMSampleBufferGetPresentationTimeStamp(sample));
  int64_t duration = micros(CMSampleBufferGetDuration(sample));
  o->duration = duration > 0 ? (uint64_t)duration : 0;
  CFArrayRef attachments =
      CMSampleBufferGetSampleAttachmentsArray(sample, false);
  o->key = 1;
  if (attachments && CFArrayGetCount(attachments)) {
    CFDictionaryRef a = CFArrayGetValueAtIndex(attachments, 0);
    o->key = CFDictionaryGetValue(a, kCMSampleAttachmentKey_NotSync) !=
             kCFBooleanTrue;
  }
  if (c->first_output &&
      !copy_avcc(CMSampleBufferGetFormatDescription(sample), o)) {
    xavi_output_free(o);
    c->callback_error = XAVI_FAILED;
    return;
  }
  c->first_output = 0;
  c->output = o;
}
static void decoded(void *ref, void *source, OSStatus status,
                    VTDecodeInfoFlags flags, CVImageBufferRef image, CMTime pts,
                    CMTime duration) {
  (void)source;
  (void)flags;
  XaviCodec *c = ref;
  if (status || !image || c->output) {
    c->callback_error = XAVI_FAILED;
    return;
  }
  size_t width = CVPixelBufferGetWidth(image),
         height = CVPixelBufferGetHeight(image);
  if (!width || !height || width > MAX_BYTES / 4 / height ||
      CVPixelBufferGetPixelFormatType(image) != kCVPixelFormatType_32BGRA) {
    c->callback_error = XAVI_UNSUPPORTED;
    return;
  }
  XaviOutput *o = output_new(width * height * 4);
  if (!o) {
    c->callback_error = XAVI_RESOURCE;
    return;
  }
  if (CVPixelBufferLockBaseAddress(image, kCVPixelBufferLock_ReadOnly)) {
    xavi_output_free(o);
    c->callback_error = XAVI_FAILED;
    return;
  }
  const uint8_t *data = CVPixelBufferGetBaseAddress(image);
  size_t stride = CVPixelBufferGetBytesPerRow(image);
  if (!data || stride < width * 4) {
    c->callback_error = XAVI_FAILED;
  } else
    for (size_t y = 0; y < height; ++y)
      memcpy(o->data + y * width * 4, data + y * stride, width * 4);
  CVPixelBufferUnlockBaseAddress(image, kCVPixelBufferLock_ReadOnly);
  if (c->callback_error) {
    xavi_output_free(o);
    return;
  }
  o->width = (uint32_t)width;
  o->height = (uint32_t)height;
  o->format = XAVI_BGRA;
  o->timestamp = micros(pts);
  int64_t d = micros(duration);
  o->duration = d > 0 ? (uint64_t)d : 0;
  c->output = o;
}

static int32_t video_open(XaviCodec *c) {
  const XaviConfig *config = &c->config;
  if (config->mode == XAVI_VIDEO_ENCODE) {
    OSStatus s = VTCompressionSessionCreate(
        NULL, (int32_t)config->width, (int32_t)config->height,
        kCMVideoCodecType_H264, NULL, NULL, NULL, encoded, c, &c->encoder);
    if (s)
      return XAVI_UNSUPPORTED;
    if (VTSessionSetProperty(c->encoder, kVTCompressionPropertyKey_RealTime,
                             kCFBooleanTrue) ||
        VTSessionSetProperty(c->encoder,
                             kVTCompressionPropertyKey_AllowFrameReordering,
                             kCFBooleanFalse) ||
        VTSessionSetProperty(c->encoder, kVTCompressionPropertyKey_ProfileLevel,
                             kVTProfileLevel_H264_Baseline_3_0) ||
        property(c->encoder, kVTCompressionPropertyKey_AverageBitRate,
                 (int64_t)config->bitrate) ||
        property(c->encoder, kVTCompressionPropertyKey_MaxKeyFrameInterval,
                 120))
      return XAVI_UNSUPPORTED;
    CFNumberRef fps =
        CFNumberCreate(NULL, kCFNumberFloat64Type, &config->framerate);
    if (!fps)
      return XAVI_RESOURCE;
    s = VTSessionSetProperty(c->encoder,
                             kVTCompressionPropertyKey_ExpectedFrameRate, fps);
    CFRelease(fps);
    if (s)
      return XAVI_UNSUPPORTED;
    return failed(VTCompressionSessionPrepareToEncodeFrames(c->encoder));
  }
  const uint8_t *p = config->description;
  size_t n = config->description_len;
  if (n < 11 || p[0] != 1 || (p[4] & 3) != 3 || (p[5] & 31) != 1)
    return XAVI_INVALID;
  size_t sn = ((size_t)p[6] << 8) | p[7];
  if (!sn || sn > n - 11 || p[8 + sn] != 1)
    return XAVI_INVALID;
  size_t pn = ((size_t)p[9 + sn] << 8) | p[10 + sn];
  if (!pn || pn != n - 11 - sn)
    return XAVI_INVALID;
  const uint8_t *sets[] = {p + 8, p + 11 + sn};
  const size_t sizes[] = {sn, pn};
  if (CMVideoFormatDescriptionCreateFromH264ParameterSets(NULL, 2, sets, sizes,
                                                          4, &c->video_format))
    return XAVI_INVALID;
  int32_t pixel = kCVPixelFormatType_32BGRA;
  CFNumberRef number = CFNumberCreate(NULL, kCFNumberSInt32Type, &pixel);
  if (!number)
    return XAVI_RESOURCE;
  const void *keys[] = {kCVPixelBufferPixelFormatTypeKey};
  const void *values[] = {number};
  CFDictionaryRef attrs =
      CFDictionaryCreate(NULL, keys, values, 1, &kCFTypeDictionaryKeyCallBacks,
                         &kCFTypeDictionaryValueCallBacks);
  CFRelease(number);
  if (!attrs)
    return XAVI_RESOURCE;
  VTDecompressionOutputCallbackRecord callback = {decoded, c};
  OSStatus s = VTDecompressionSessionCreate(NULL, c->video_format, NULL, attrs,
                                            &callback, &c->decoder);
  CFRelease(attrs);
  return s ? XAVI_UNSUPPORTED : XAVI_OK;
}

static AudioStreamBasicDescription pcm(const XaviConfig *c) {
  AudioStreamBasicDescription d = {0};
  d.mSampleRate = c->sample_rate;
  d.mFormatID = kAudioFormatLinearPCM;
  d.mFormatFlags = kAudioFormatFlagIsSignedInteger | kAudioFormatFlagIsPacked |
                   kAudioFormatFlagsNativeEndian;
  d.mFramesPerPacket = 1;
  d.mChannelsPerFrame = c->channels;
  d.mBitsPerChannel = 16;
  d.mBytesPerFrame = d.mBytesPerPacket = 2 * c->channels;
  return d;
}
static int32_t audio_open(XaviCodec *c) {
  AudioStreamBasicDescription raw = pcm(&c->config), compressed = {0};
  compressed.mSampleRate = c->config.sample_rate;
  compressed.mChannelsPerFrame = c->config.channels;
  compressed.mFormatID = kAudioFormatMPEG4AAC;
  compressed.mFramesPerPacket = 1024;
  int encode = c->config.mode == XAVI_AUDIO_ENCODE;
  OSStatus s = AudioConverterNew(encode ? &raw : &compressed,
                                 encode ? &compressed : &raw, &c->audio);
  if (s)
    return XAVI_UNSUPPORTED;
  if (encode) {
    uint32_t bitrate = (uint32_t)c->config.bitrate;
    if (AudioConverterSetProperty(c->audio, kAudioConverterEncodeBitRate,
                                  sizeof(bitrate), &bitrate))
      return XAVI_UNSUPPORTED;
    uint32_t size = sizeof(c->packet_capacity);
    if (AudioConverterGetProperty(
            c->audio, kAudioConverterPropertyMaximumOutputPacketSize, &size,
            &c->packet_capacity))
      return XAVI_FAILED;
  } else
    c->packet_capacity = 1024 * 2 * c->config.channels;
  return c->packet_capacity > 0 && c->packet_capacity <= MAX_BYTES
             ? XAVI_OK
             : XAVI_RESOURCE;
}

XaviCodec *xavi_codec_create(const XaviConfig *config, int32_t *status) {
  *status = XAVI_RESOURCE;
  XaviCodec *c = calloc(1, sizeof(*c));
  if (!c)
    return NULL;
  c->config = *config;
  c->first_output = 1;
  *status = config->mode <= XAVI_AUDIO_DECODE ? audio_open(c) : video_open(c);
  // Description memory is borrowed only while opening the framework object.
  c->config.description = NULL;
  c->config.description_len = 0;
  if (*status) {
    xavi_codec_destroy(c);
    return NULL;
  }
  return c;
}
void xavi_codec_destroy(XaviCodec *c) {
  if (!c)
    return;
  if (c->encoder) {
    VTCompressionSessionInvalidate(c->encoder);
    CFRelease(c->encoder);
  }
  if (c->decoder) {
    VTDecompressionSessionInvalidate(c->decoder);
    CFRelease(c->decoder);
  }
  if (c->video_format)
    CFRelease(c->video_format);
  if (c->audio)
    AudioConverterDispose(c->audio);
  xavi_output_free(c->output);
  free(c->input);
  free(c->retired_input);
  free(c);
}

static int32_t video_send(XaviCodec *c, const uint8_t *data, size_t len,
                          int64_t pts, uint64_t duration, uint32_t format,
                          int32_t key) {
  CMTime timestamp = CMTimeMake(pts, 1000000),
         time =
             duration ? CMTimeMake((int64_t)duration, 1000000) : kCMTimeInvalid;
  if (c->encoder) {
    size_t w = c->config.width, h = c->config.height;
    OSType pixel = format == XAVI_BGRA
                       ? kCVPixelFormatType_32BGRA
                       : kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange;
    size_t expected = format == XAVI_BGRA ? w * h * 4 : w * h * 3 / 2;
    if (len != expected || (format != XAVI_BGRA && format != XAVI_NV12))
      return XAVI_INVALID;
    CVPixelBufferRef image = NULL;
    if (CVPixelBufferCreate(NULL, w, h, pixel, NULL, &image))
      return XAVI_RESOURCE;
    if (CVPixelBufferLockBaseAddress(image, 0)) {
      CFRelease(image);
      return XAVI_FAILED;
    }
    if (format == XAVI_BGRA) {
      for (size_t y = 0; y < h; ++y)
        memcpy((uint8_t *)CVPixelBufferGetBaseAddress(image) +
                   y * CVPixelBufferGetBytesPerRow(image),
               data + y * w * 4, w * 4);
    } else
      for (size_t p = 0; p < 2; ++p) {
        uint8_t *dest = CVPixelBufferGetBaseAddressOfPlane(image, p);
        size_t stride = CVPixelBufferGetBytesPerRowOfPlane(image, p),
               rows = p ? h / 2 : h;
        const uint8_t *src = data + (p ? w * h : 0);
        for (size_t y = 0; y < rows; ++y)
          memcpy(dest + y * stride, src + y * w, w);
      }
    CVPixelBufferUnlockBaseAddress(image, 0);
    CFDictionaryRef options = NULL;
    if (key) {
      const void *keys[] = {kVTEncodeFrameOptionKey_ForceKeyFrame};
      const void *values[] = {kCFBooleanTrue};
      options = CFDictionaryCreate(NULL, keys, values, 1,
                                   &kCFTypeDictionaryKeyCallBacks,
                                   &kCFTypeDictionaryValueCallBacks);
      if (!options) {
        CFRelease(image);
        return XAVI_RESOURCE;
      }
    }
    OSStatus s = VTCompressionSessionEncodeFrame(c->encoder, image, timestamp,
                                                 time, options, NULL, NULL);
    if (options)
      CFRelease(options);
    CFRelease(image);
    if (!s)
      s = VTCompressionSessionCompleteFrames(c->encoder, kCMTimeInvalid);
    return s ? XAVI_FAILED : c->callback_error;
  }
  CMBlockBufferRef block = NULL;
  CMSampleBufferRef sample = NULL;
  OSStatus s = CMBlockBufferCreateWithMemoryBlock(NULL, NULL, len, NULL, NULL,
                                                  0, len, 0, &block);
  if (s)
    return XAVI_RESOURCE;
  s = CMBlockBufferReplaceDataBytes(data, block, 0, len);
  CMSampleTimingInfo timing = {time, timestamp, kCMTimeInvalid};
  if (!s)
    s = CMSampleBufferCreateReady(NULL, block, c->video_format, 1, 1, &timing,
                                  1, &len, &sample);
  CFRelease(block);
  if (!s)
    s = VTDecompressionSessionDecodeFrame(c->decoder, sample, 0, NULL, NULL);
  if (sample)
    CFRelease(sample);
  if (!s)
    s = VTDecompressionSessionWaitForAsynchronousFrames(c->decoder);
  return s ? XAVI_FAILED : c->callback_error;
}

int32_t xavi_codec_send(XaviCodec *c, const uint8_t *data, size_t len,
                        int64_t timestamp, uint64_t duration, uint32_t frames,
                        uint32_t format, int32_t key) {
  (void)frames;
  if (c->eos || len == 0 || len > MAX_BYTES || duration > INT64_MAX)
    return XAVI_INVALID;
  if (c->output || c->input_pos < c->input_len)
    return XAVI_PENDING;
  if (!c->audio)
    return video_send(c, data, len, timestamp, duration, format, key);
  uint8_t *copy = malloc(len);
  if (!copy)
    return XAVI_RESOURCE;
  memcpy(copy, data, len);
  // AudioConverter may keep the most recently supplied input until its next
  // input callback. Keep that allocation alive even between fill calls.
  c->retired_input = c->input;
  c->input = copy;
  c->input_len = len;
  c->input_pos = 0;
  if (!c->have_origin) {
    c->origin = timestamp;
    c->have_origin = 1;
  }
  return XAVI_OK;
}
static OSStatus input_audio(AudioConverterRef converter, UInt32 *packets,
                            AudioBufferList *buffers,
                            AudioStreamPacketDescription **description,
                            void *ref) {
  (void)converter;
  XaviCodec *c = ref;
  free(c->retired_input);
  c->retired_input = NULL;
  size_t remaining = c->input_len - c->input_pos;
  buffers->mNumberBuffers = 1;
  buffers->mBuffers[0].mNumberChannels = c->config.channels;
  if (!remaining) {
    *packets = 0;
    buffers->mBuffers[0].mDataByteSize = 0;
    buffers->mBuffers[0].mData = NULL;
    return c->eos ? 0 : NEED_INPUT;
  }
  size_t bytes;
  if (c->config.mode == XAVI_AUDIO_ENCODE) {
    uint32_t stride = c->config.channels * 2;
    if (*packets > remaining / stride)
      *packets = (uint32_t)(remaining / stride);
    bytes = (size_t)*packets * stride;
  } else {
    *packets = 1;
    bytes = remaining;
    c->input_packet.mStartOffset = 0;
    c->input_packet.mDataByteSize = (uint32_t)bytes;
    c->input_packet.mVariableFramesInPacket = 1024;
    if (description)
      *description = &c->input_packet;
  }
  buffers->mBuffers[0].mData = c->input + c->input_pos;
  buffers->mBuffers[0].mDataByteSize = (uint32_t)bytes;
  c->input_pos += bytes;
  return 0;
}
int32_t xavi_codec_receive(XaviCodec *c, XaviOutput **output) {
  *output = NULL;
  if (c->callback_error)
    return c->callback_error;
  if (c->output) {
    *output = c->output;
    c->output = NULL;
    return XAVI_OK;
  }
  if (!c->audio)
    return c->eos ? XAVI_END : XAVI_PENDING;
  XaviOutput *o = output_new(c->packet_capacity);
  if (!o)
    return XAVI_RESOURCE;
  AudioBufferList buffers = {
      .mNumberBuffers = 1,
      .mBuffers = {{c->config.channels, c->packet_capacity, o->data}}};
  uint32_t packets = c->config.mode == XAVI_AUDIO_ENCODE ? 1 : 1024;
  AudioStreamPacketDescription desc = {0};
  OSStatus s = AudioConverterFillComplexBuffer(
      c->audio, input_audio, c, &packets, &buffers,
      c->config.mode == XAVI_AUDIO_ENCODE ? &desc : NULL);
  if (s && s != NEED_INPUT) {
    xavi_output_free(o);
    return XAVI_FAILED;
  }
  if (!packets || !buffers.mBuffers[0].mDataByteSize) {
    xavi_output_free(o);
    return c->eos && !s ? XAVI_END : XAVI_PENDING;
  }
  o->len = buffers.mBuffers[0].mDataByteSize;
  o->frames = c->config.mode == XAVI_AUDIO_ENCODE ? 1024 : packets;
  o->sample_rate = c->config.sample_rate;
  o->channels = c->config.channels;
  o->format = XAVI_S16;
  o->key = 1;
  // Use integer sample-clock arithmetic rather than repeatedly adding rounded
  // packet durations. Timestamps describe the converter's complete output.
  if (c->output_frames > UINT64_MAX / 1000000) {
    xavi_output_free(o);
    return XAVI_INVALID;
  }
  uint64_t elapsed = c->output_frames * 1000000 / c->config.sample_rate;
  if (elapsed > INT64_MAX || c->origin > INT64_MAX - (int64_t)elapsed) {
    xavi_output_free(o);
    return XAVI_INVALID;
  }
  o->timestamp = c->origin + (int64_t)elapsed;
  o->duration = (uint64_t)o->frames * 1000000 / c->config.sample_rate;
  c->output_frames += o->frames;
  *output = o;
  return XAVI_OK;
}
int32_t xavi_codec_drain(XaviCodec *c) {
  c->eos = 1;
  if (c->encoder &&
      VTCompressionSessionCompleteFrames(c->encoder, kCMTimeInvalid))
    return XAVI_FAILED;
  if (c->decoder &&
      (VTDecompressionSessionFinishDelayedFrames(c->decoder) ||
       VTDecompressionSessionWaitForAsynchronousFrames(c->decoder)))
    return XAVI_FAILED;
  return c->callback_error;
}
