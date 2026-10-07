#import <AVFoundation/AVFoundation.h>
#include <math.h>
#include <stdio.h>
#include <stdatomic.h>

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
  return 0;
}
void *xavi_player_open(const char *path, char *error) {
  @autoreleasepool {
    if (main_thread(error)) return NULL;
    @try {
      XaviPlayback *c = [XaviPlayback new];
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
      case 0: [c.player play]; break;
      case 1: [c.player pause]; break;
      case 2: {
        c.ended = NO;
        c.seeking = YES;
        NSUInteger generation = ++c.seekGeneration;
        __weak XaviPlayback *weak = c;
        [c.player seekToTime:CMTimeMakeWithSeconds(value, 1000000)
            toleranceBefore:kCMTimeZero toleranceAfter:kCMTimeZero
            completionHandler:^(BOOL finished) {
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
