#include "../native.h"
#import <AVFoundation/AVFoundation.h>
#include <stdio.h>

// Each entry point uses a local autorelease pool and catches Objective-C
// exceptions. Neither autoreleased objects nor exceptions cross the Rust ABI.
@interface XaviWriter : NSObject
@property(nonatomic, strong) AVAssetWriter *writer;
@property(nonatomic, strong) AVAssetWriterInput *audio;
@property(nonatomic, strong) AVAssetWriterInput *video;
@property(nonatomic) CMAudioFormatDescriptionRef audioFormat;
@property(nonatomic) CMVideoFormatDescriptionRef videoFormat;
@property(nonatomic) BOOL finishing;
@end
@implementation XaviWriter
- (void)dealloc {
  if (_writer.status == AVAssetWriterStatusWriting ||
      _writer.status == AVAssetWriterStatusUnknown)
    [_writer cancelWriting];
  if (_audioFormat)
    CFRelease(_audioFormat);
  if (_videoFormat)
    CFRelease(_videoFormat);
}
@end
static int fail(char *error, NSString *message) {
  snprintf(error, 512, "%s", (message ?: @"native writer failed").UTF8String);
  return -1;
}
static int format_error(char *error, OSStatus status) {
  return fail(error,
              [NSString stringWithFormat:@"CoreMedia format/sample error %d",
                                         (int)status]);
}
void *xavi_mux_open(const char *path, const XaviConfig *audio,
                    const XaviConfig *video, char *error) {
  @autoreleasepool {
    @try {
      XaviWriter *c = [XaviWriter new];
      NSError *e = nil;
      c.writer = [[AVAssetWriter alloc]
          initWithURL:[NSURL
                          fileURLWithPath:[NSString stringWithUTF8String:path]]
             fileType:AVFileTypeMPEG4
                error:&e];
      if (!c.writer) {
        fail(error, e.description);
        return NULL;
      }
      if (audio) {
        AudioStreamBasicDescription d = {0};
        d.mSampleRate = audio->sample_rate;
        d.mChannelsPerFrame = audio->channels;
        d.mFormatID = kAudioFormatMPEG4AAC;
        d.mFormatFlags = kMPEG4Object_AAC_LC;
        d.mFramesPerPacket = 1024;
        CMAudioFormatDescriptionRef f = NULL;
        // CoreMedia needs an MPEG-4 ES_Descriptor cookie, not the bare
        // AudioSpecificConfig exposed by our codec API. Lengths below describe
        // ES(25), DecoderConfig(17), DecoderSpecificInfo(2) and SLConfig(1).
        const uint8_t cookie[] = {3,
                                  25,
                                  0,
                                  0,
                                  0,
                                  4,
                                  17,
                                  0x40,
                                  0x15,
                                  0,
                                  0,
                                  0,
                                  0,
                                  0,
                                  0,
                                  0,
                                  0,
                                  0,
                                  0,
                                  0,
                                  5,
                                  2,
                                  audio->description[0],
                                  audio->description[1],
                                  6,
                                  1,
                                  2};
        OSStatus s = CMAudioFormatDescriptionCreate(
            NULL, &d, 0, NULL, sizeof(cookie), cookie, NULL, &f);
        if (s) {
          format_error(error, s);
          return NULL;
        }
        c.audioFormat = f;
        c.audio =
            [AVAssetWriterInput assetWriterInputWithMediaType:AVMediaTypeAudio
                                               outputSettings:nil
                                             sourceFormatHint:f];
        c.audio.expectsMediaDataInRealTime = NO;
        if (![c.writer canAddInput:c.audio]) {
          fail(error, @"AAC passthrough is unavailable");
          return NULL;
        }
        [c.writer addInput:c.audio];
      }
      if (video) {
        const uint8_t *p = video->description;
        // The Rust boundary has validated this single-SPS/single-PPS avcC.
        size_t sn = ((size_t)p[6] << 8) | p[7];
        size_t pn = ((size_t)p[9 + sn] << 8) | p[10 + sn];
        const uint8_t *sets[] = {p + 8, p + 11 + sn};
        const size_t lengths[] = {sn, pn};
        CMVideoFormatDescriptionRef f = NULL;
        OSStatus s = CMVideoFormatDescriptionCreateFromH264ParameterSets(
            NULL, 2, sets, lengths, 4, &f);
        if (s) {
          format_error(error, s);
          return NULL;
        }
        c.videoFormat = f;
        CMVideoDimensions size = CMVideoFormatDescriptionGetDimensions(f);
        if (size.width != (int32_t)video->width ||
            size.height != (int32_t)video->height) {
          fail(error, @"AVC dimensions disagree with track configuration");
          return NULL;
        }
        c.video =
            [AVAssetWriterInput assetWriterInputWithMediaType:AVMediaTypeVideo
                                               outputSettings:nil
                                             sourceFormatHint:f];
        c.video.expectsMediaDataInRealTime = NO;
        if (![c.writer canAddInput:c.video]) {
          fail(error, @"AVC passthrough is unavailable");
          return NULL;
        }
        [c.writer addInput:c.video];
      }
      if (![c.writer startWriting]) {
        fail(error, c.writer.error.description);
        return NULL;
      }
      [c.writer startSessionAtSourceTime:kCMTimeZero];
      return (void *)CFBridgingRetain(c);
    } @catch (NSException *e) {
      fail(error, e.reason);
      return NULL;
    }
  }
}
int xavi_mux_write(void *ctx, int track, const uint8_t *data, size_t len,
                   int64_t timestamp, uint64_t duration, bool key,
                   char *error) {
  @autoreleasepool {
    @try {
      XaviWriter *c = (__bridge XaviWriter *)ctx;
      if (c.writer.status != AVAssetWriterStatusWriting)
        return fail(error, c.writer.error.description);
      AVAssetWriterInput *input = track == 0 ? c.audio : c.video;
      if (!input.readyForMoreMediaData)
        return 1;
      CMBlockBufferRef block = NULL;
      CMSampleBufferRef sample = NULL;
      OSStatus s = CMBlockBufferCreateWithMemoryBlock(NULL, NULL, len, NULL,
                                                      NULL, 0, len, 0, &block);
      if (s)
        return format_error(error, s);
      s = CMBlockBufferReplaceDataBytes(data, block, 0, len);
      CMTime pts = CMTimeMake(timestamp, 1000000);
      if (!s && track == 0) {
        // AAC timing is a sample clock. Microsecond rounding otherwise
        // creates sub-sample gaps that AVAssetWriter may fill with silence.
        const AudioStreamBasicDescription *asbd =
            CMAudioFormatDescriptionGetStreamBasicDescription(c.audioFormat);
        pts = CMTimeConvertScale(pts, (int32_t)asbd->mSampleRate,
                                 kCMTimeRoundingMethod_RoundHalfAwayFromZero);
        AudioStreamPacketDescription packet = {0, 1024, (uint32_t)len};
        s = CMAudioSampleBufferCreateReadyWithPacketDescriptions(
            NULL, block, c.audioFormat, 1, pts, &packet, &sample);
      } else if (!s) {
        CMSampleTimingInfo timing = {CMTimeMake((int64_t)duration, 1000000),
                                     pts, pts};
        s = CMSampleBufferCreateReady(NULL, block, c.videoFormat, 1, 1, &timing,
                                      1, &len, &sample);
      }
      CFRelease(block);
      if (s)
        return format_error(error, s);
      if (track == 0) {
        CFDictionaryRef trim = CMTimeCopyAsDictionary(kCMTimeZero, NULL);
        CMSetAttachment(sample,
                        kCMSampleBufferAttachmentKey_TrimDurationAtStart, trim,
                        kCMAttachmentMode_ShouldPropagate);
        CMSetAttachment(sample, kCMSampleBufferAttachmentKey_TrimDurationAtEnd,
                        trim, kCMAttachmentMode_ShouldPropagate);
        CFRelease(trim);
      }
      if (track != 0) {
        CFArrayRef list = CMSampleBufferGetSampleAttachmentsArray(sample, true);
        CFMutableDictionaryRef a =
            (CFMutableDictionaryRef)CFArrayGetValueAtIndex(list, 0);
        CFDictionarySetValue(a, kCMSampleAttachmentKey_NotSync,
                             key ? kCFBooleanFalse : kCFBooleanTrue);
      }
      // append retains any media needed after return. Release our CF sample
      // even if the framework throws an Objective-C exception.
      BOOL accepted = NO;
      @try {
        accepted = [input appendSampleBuffer:sample];
      } @finally {
        CFRelease(sample);
      }
      return accepted ? 0 : fail(error, c.writer.error.description);
    } @catch (NSException *e) {
      return fail(error, e.reason);
    }
  }
}
int xavi_mux_end(void *ctx, int track, char *error) {
  @autoreleasepool {
    @try {
      XaviWriter *c = (__bridge XaviWriter *)ctx;
      [(track == 0 ? c.audio : c.video) markAsFinished];
      return 0;
    } @catch (NSException *e) {
      return fail(error, e.reason);
    }
  }
}
int xavi_mux_finish(void *ctx, char *error) {
  @autoreleasepool {
    @try {
      XaviWriter *c = (__bridge XaviWriter *)ctx;
      if (!c.finishing) {
        c.finishing = YES;
        [c.writer finishWritingWithCompletionHandler:^{
        }];
      }
      switch (c.writer.status) {
      case AVAssetWriterStatusCompleted:
        return 0;
      case AVAssetWriterStatusWriting:
        return 1;
      default:
        return fail(error, c.writer.error.description);
      }
    } @catch (NSException *e) {
      return fail(error, e.reason);
    }
  }
}
void xavi_mux_drop(void *ctx) {
  @autoreleasepool {
    (void)CFBridgingRelease(ctx);
  }
}
