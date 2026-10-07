#import <AVFoundation/AVFoundation.h>
#import <MediaToolbox/MediaToolbox.h>
#include <math.h>
#include <stdio.h>
#include <stdatomic.h>
#include <stdbool.h>

extern void *xavi_eq_stream_new(const void *control);
extern void xavi_eq_stream_drop(void *stream);
extern bool xavi_eq_process(void *stream, AudioBuffer *buffers, uint32_t count,
    uint32_t frames, double rate, uint32_t channels, bool floating, bool planar, bool reset);
typedef struct {
  void *stream;
  AudioStreamBasicDescription format;
} XaviAudioTap;
static void tap_init(MTAudioProcessingTapRef tap, void *client, void **storage) {
  (void)tap;
  *storage = client;
}
static void tap_finalize(MTAudioProcessingTapRef tap) {
  XaviAudioTap *state = MTAudioProcessingTapGetStorage(tap);
  xavi_eq_stream_drop(state->stream);
  free(state);
}
static void tap_prepare(MTAudioProcessingTapRef tap, CMItemCount maxFrames, const AudioStreamBasicDescription *format) {
  (void)maxFrames;
  ((XaviAudioTap *)MTAudioProcessingTapGetStorage(tap))->format = *format;
}
static void tap_unprepare(MTAudioProcessingTapRef tap) { (void)tap; }
static void tap_process(MTAudioProcessingTapRef tap, CMItemCount requested,
    MTAudioProcessingTapFlags flags, AudioBufferList *buffers,
    CMItemCount *provided, MTAudioProcessingTapFlags *outFlags) {
  (void)flags;
  OSStatus status = MTAudioProcessingTapGetSourceAudio(tap, requested, buffers, outFlags, NULL, provided);
  if (status != noErr) { *provided = 0; return; }
  XaviAudioTap *state = MTAudioProcessingTapGetStorage(tap);
  AudioStreamBasicDescription f = state->format;
  bool floating = (f.mFormatFlags & kAudioFormatFlagIsFloat) != 0;
  bool valid = f.mFormatID == kAudioFormatLinearPCM &&
      !(f.mFormatFlags & kAudioFormatFlagIsBigEndian) &&
      ((floating && f.mBitsPerChannel == 32) ||
       (!floating && (f.mFormatFlags & kAudioFormatFlagIsSignedInteger) && f.mBitsPerChannel == 16));
  if (*provided <= 0) return;
  if (!xavi_eq_process(state->stream, buffers->mBuffers, buffers->mNumberBuffers,
      (uint32_t)*provided, valid ? f.mSampleRate : 0, f.mChannelsPerFrame,
      floating, (f.mFormatFlags & kAudioFormatFlagIsNonInterleaved) != 0,
      (*outFlags & kMTAudioProcessingTapFlag_StartOfStream) != 0)) {
    for (UInt32 i = 0; i < buffers->mNumberBuffers; ++i)
      memset(buffers->mBuffers[i].mData, 0, buffers->mBuffers[i].mDataByteSize);
  }
}

// AVPlayer and its notification state stay on the main thread. The Rust owner
// may be dropped elsewhere; destruction is then dispatched to that thread.
@interface XaviPlayback : NSObject {
@public
  _Atomic(NSUInteger) completedSeekGeneration;
}
@property(nonatomic, strong) AVPlayer *player;
@property(nonatomic, strong) AVPlayerItemVideoOutput *output;
@property(nonatomic, strong) id endObserver;
@property(nonatomic, strong) id failureObserver;
@property(nonatomic, strong) NSError *failure;
@property(nonatomic) BOOL ended;
@property(nonatomic) BOOL seeking;
@property(nonatomic) BOOL closed;
@property(nonatomic) NSUInteger seekGeneration;
@property(nonatomic) void *equalizer;
@property(nonatomic) BOOL tapsReady;
@property(nonatomic) BOOL wantsPlay;
- (void)close;
@end
@implementation XaviPlayback
- (void)dealloc { [self close]; }
- (void)close {
  if (_closed) return;
  _closed = YES;
  _seekGeneration++;
  [_player pause];
  [_player.currentItem cancelPendingSeeks];
  if (_endObserver) [[NSNotificationCenter defaultCenter] removeObserver:_endObserver];
  if (_failureObserver) [[NSNotificationCenter defaultCenter] removeObserver:_failureObserver];
  _endObserver = nil;
  _failureObserver = nil;
  [_player replaceCurrentItemWithPlayerItem:nil];
  _player = nil;
  _output = nil;
  if (_equalizer) { xavi_eq_stream_drop(_equalizer); _equalizer = NULL; }
}
@end

typedef struct {
  int32_t state;
  double position;
  double duration;
  double volume;
} XaviPlaybackInfo;
typedef struct {
  CVPixelBufferRef buffer;
  const uint8_t *data;
  size_t len;
  uint32_t width, height, stride;
  int64_t timestamp;
} XaviPlaybackFrame;

static int fail(char *error, NSString *message) {
  snprintf(error, 512, "%s", (message ?: @"native playback failed").UTF8String);
  return -1;
}
static int main_thread(char *error) {
  if ([NSThread isMainThread]) return 0;
  fail(error, @"media playback must be driven on the main thread");
  return -2;
}
int xavi_player_main_thread(void) { return [NSThread isMainThread] ? 1 : 0; }
static int check(XaviPlayback *c, char *error) {
  if (c.closed) return fail(error, @"player is closed");
  if (c.seeking && atomic_load_explicit(&c->completedSeekGeneration, memory_order_acquire) == c.seekGeneration)
    c.seeking = NO;
  NSError *e = c.failure ?: c.player.error ?: c.player.currentItem.error;
  if (e) return fail(error, e.localizedDescription);
  if (!c.tapsReady && c.player.currentItem.status == AVPlayerItemStatusReadyToPlay) {
    NSMutableArray *parameters = [NSMutableArray array];
    for (AVPlayerItemTrack *track in c.player.currentItem.tracks) {
      if (![track.assetTrack.mediaType isEqualToString:AVMediaTypeAudio]) continue;
      XaviAudioTap *state = calloc(1, sizeof(XaviAudioTap));
      if (!state) return fail(error, @"could not allocate equalizer tap");
      // The retained Rust seed owns the shared control; each track owns history.
      extern void *xavi_eq_stream_fork(void *seed);
      state->stream = xavi_eq_stream_fork(c.equalizer);
      MTAudioProcessingTapCallbacks callbacks = { kMTAudioProcessingTapCallbacksVersion_0,
          state, tap_init, tap_finalize, tap_prepare, tap_unprepare, tap_process };
      MTAudioProcessingTapRef tap = NULL;
      OSStatus status = MTAudioProcessingTapCreate(kCFAllocatorDefault, &callbacks,
          kMTAudioProcessingTapCreationFlag_PostEffects, &tap);
      if (status != noErr) {
        xavi_eq_stream_drop(state->stream); free(state);
        return fail(error, @"could not install audio equalizer tap");
      }
      AVMutableAudioMixInputParameters *p = [AVMutableAudioMixInputParameters audioMixInputParametersWithTrack:track.assetTrack];
      p.audioTapProcessor = tap;
      CFRelease(tap);
      [parameters addObject:p];
    }
    AVMutableAudioMix *mix = [AVMutableAudioMix audioMix];
    mix.inputParameters = parameters;
    c.player.currentItem.audioMix = mix;
    c.tapsReady = YES;
    if (c.wantsPlay) [c.player play];
  }
  return 0;
}
void *xavi_player_open(const char *path, const void *control, char *error) {
  @autoreleasepool {
    if (main_thread(error)) return NULL;
    @try {
      XaviPlayback *c = [XaviPlayback new];
      c.equalizer = xavi_eq_stream_new(control);
      atomic_init(&c->completedSeekGeneration, 0);
      NSURL *url = [NSURL fileURLWithPath:[NSString stringWithUTF8String:path]];
      AVURLAsset *asset = [AVURLAsset URLAssetWithURL:url options:nil];
      AVPlayerItem *item = [AVPlayerItem playerItemWithAsset:asset];
      c.output = [[AVPlayerItemVideoOutput alloc] initWithPixelBufferAttributes:@{
        (id)kCVPixelBufferPixelFormatTypeKey : @(kCVPixelFormatType_32BGRA)
      }];
      // Only custom-render video; AVPlayer still renders audio on its clock.
      c.output.suppressesPlayerRendering = YES;
      [item addOutput:c.output];
      c.player = [AVPlayer playerWithPlayerItem:item];
      c.player.actionAtItemEnd = AVPlayerActionAtItemEndPause;
      c.player.volume = 1.0;
      __weak XaviPlayback *weak = c;
      c.endObserver = [[NSNotificationCenter defaultCenter]
          addObserverForName:AVPlayerItemDidPlayToEndTimeNotification object:item
          queue:NSOperationQueue.mainQueue usingBlock:^(NSNotification *note) {
            (void)note;
            XaviPlayback *p = weak;
            if (p && !p.closed && !p.seeking) p.ended = YES;
          }];
      c.failureObserver = [[NSNotificationCenter defaultCenter]
          addObserverForName:AVPlayerItemFailedToPlayToEndTimeNotification object:item
          queue:NSOperationQueue.mainQueue usingBlock:^(NSNotification *note) {
            XaviPlayback *p = weak;
            if (p && !p.closed)
              p.failure = note.userInfo[AVPlayerItemFailedToPlayToEndTimeErrorKey];
          }];
      return (void *)CFBridgingRetain(c);
    } @catch (NSException *e) {
      fail(error, e.reason);
      return NULL;
    }
  }
}
// Commands: 0 play, 1 pause, 2 seek (seconds), 3 volume (0..1).
int xavi_player_command(void *ctx, int command, double value, char *error) {
  @autoreleasepool {
    int status = main_thread(error);
    if (status) return status;
    @try {
      XaviPlayback *c = (__bridge XaviPlayback *)ctx;
      if (check(c, error)) return -1;
      switch (command) {
      case 0: c.wantsPlay = YES; if (c.tapsReady) [c.player play]; break;
      case 1: c.wantsPlay = NO; [c.player pause]; break;
      case 2: {
        c.ended = NO;
        c.seeking = YES;
        NSUInteger generation = ++c.seekGeneration;
        __weak XaviPlayback *weak = c;
        [c.player seekToTime:CMTimeMakeWithSeconds(value, 1000000)
            toleranceBefore:kCMTimeZero toleranceAfter:kCMTimeZero
            completionHandler:^(BOOL finished) {
              (void)finished;
              // AVFoundation can finish on a private queue. Publish only the
              // generation here; the next main-thread poll updates the state.
              // A pumped application loop need not drain GCD's main queue.
              XaviPlayback *p = weak;
              if (p) {
                NSUInteger seen = atomic_load_explicit(&p->completedSeekGeneration, memory_order_relaxed);
                while (seen < generation && !atomic_compare_exchange_weak_explicit(
                    &p->completedSeekGeneration, &seen, generation,
                    memory_order_release, memory_order_relaxed)) {}
              }
            }];
        break;
      }
      case 3: c.player.volume = (float)value; break;
      default: return fail(error, @"unknown playback command");
      }
      return 0;
    } @catch (NSException *e) { return fail(error, e.reason); }
  }
}
int xavi_player_info(void *ctx, XaviPlaybackInfo *out, char *error) {
  @autoreleasepool {
    int status = main_thread(error);
    if (status) return status;
    @try {
      XaviPlayback *c = (__bridge XaviPlayback *)ctx;
      if (check(c, error)) return -1;
      AVPlayerItem *item = c.player.currentItem;
      double position = CMTimeGetSeconds(c.player.currentTime);
      double duration = CMTimeGetSeconds(item.duration);
      out->position = isfinite(position) ? fmax(0, position) : 0;
      out->duration = isfinite(duration) ? fmax(0, duration) : 0;
      out->volume = c.player.volume;
      if (item.status != AVPlayerItemStatusReadyToPlay) out->state = 0;
      else if (c.seeking) out->state = 3;
      else if (c.ended) out->state = 4;
      else if (c.player.timeControlStatus == AVPlayerTimeControlStatusWaitingToPlayAtSpecifiedRate) out->state = 3;
      else if (c.player.timeControlStatus == AVPlayerTimeControlStatusPlaying) out->state = 2;
      else out->state = 1;
      return 0;
    } @catch (NSException *e) { return fail(error, e.reason); }
  }
}
// 1 = new locked pixel buffer, 0 = no new frame, negative = failure.
int xavi_player_frame(void *ctx, XaviPlaybackFrame *out, char *error) {
  @autoreleasepool {
    int status = main_thread(error);
    if (status) return status;
    @try {
      XaviPlayback *c = (__bridge XaviPlayback *)ctx;
      if (check(c, error)) return -1;
      // Pulling output during a paused seek lets the video pipeline finish
      // that seek; waiting for completion before polling can stall it.
      if (c.player.currentItem.status != AVPlayerItemStatusReadyToPlay) return 0;
      CMTime time = c.player.currentTime;
      if (![c.output hasNewPixelBufferForItemTime:time]) return 0;
      CMTime pts = kCMTimeInvalid;
      CVPixelBufferRef buffer = [c.output copyPixelBufferForItemTime:time itemTimeForDisplay:&pts];
      if (!buffer) return 0;
      size_t width = CVPixelBufferGetWidth(buffer), height = CVPixelBufferGetHeight(buffer);
      size_t stride = CVPixelBufferGetBytesPerRow(buffer);
      if (CVPixelBufferGetPixelFormatType(buffer) != kCVPixelFormatType_32BGRA ||
          !width || !height || stride < width * 4 ||
          width > UINT32_MAX || height > UINT32_MAX || stride > UINT32_MAX ||
          height > (64 * 1024 * 1024) / stride) {
        CFRelease(buffer);
        return fail(error, @"decoded frame is not BGRA or exceeds 64 MiB");
      }
      if (CVPixelBufferLockBaseAddress(buffer, kCVPixelBufferLock_ReadOnly) != kCVReturnSuccess) {
        CFRelease(buffer);
        return fail(error, @"could not lock decoded video frame");
      }
      out->buffer = buffer;
      out->data = CVPixelBufferGetBaseAddress(buffer);
      out->len = stride * height;
      out->width = (uint32_t)width;
      out->height = (uint32_t)height;
      out->stride = (uint32_t)stride;
      out->timestamp = CMTimeConvertScale(CMTIME_IS_NUMERIC(pts) ? pts : time,
          1000000, kCMTimeRoundingMethod_RoundHalfAwayFromZero).value;
      return 1;
    } @catch (NSException *e) { return fail(error, e.reason); }
  }
}
void xavi_player_frame_release(XaviPlaybackFrame *frame) {
  if (!frame->buffer) return;
  CVPixelBufferUnlockBaseAddress(frame->buffer, kCVPixelBufferLock_ReadOnly);
  CFRelease(frame->buffer);
  frame->buffer = NULL;
}
void xavi_player_drop(void *ctx) {
  @autoreleasepool {
    XaviPlayback *c = CFBridgingRelease(ctx);
    if ([NSThread isMainThread]) [c close];
    else dispatch_async(dispatch_get_main_queue(), ^{ [c close]; });
  }
}
