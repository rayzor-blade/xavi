#include <stddef.h>
#include <stdint.h>

// Private, versioned-by-source ABI: framework objects never cross into Rust.
enum {
  XAVI_AUDIO_ENCODE = 1,
  XAVI_AUDIO_DECODE,
  XAVI_VIDEO_ENCODE,
  XAVI_VIDEO_DECODE
};
enum {
  XAVI_OK = 0,
  XAVI_PENDING = 1,
  XAVI_END = 2,
  XAVI_UNSUPPORTED = -1,
  XAVI_INVALID = -2,
  XAVI_RESOURCE = -3,
  XAVI_FAILED = -4
};
enum { XAVI_S16 = 1, XAVI_BGRA = 2, XAVI_NV12 = 3 };
typedef struct {
  int32_t mode;
  uint32_t sample_rate, channels, width, height;
  uint64_t bitrate;
  double framerate;
  const uint8_t *description;
  size_t description_len;
} XaviConfig;

typedef struct {
  uint8_t *data;
  size_t len;
  uint8_t *description;
  size_t description_len;
  int64_t timestamp;
  uint64_t duration;
  uint32_t format, frames, sample_rate, channels, width, height;
  int32_t key;
} XaviOutput;

typedef struct XaviCodec XaviCodec;
XaviCodec *xavi_codec_create(const XaviConfig *config, int32_t *status);
void xavi_codec_destroy(XaviCodec *codec);
int32_t xavi_codec_send(XaviCodec *codec, const uint8_t *data, size_t len,
                        int64_t timestamp, uint64_t duration, uint32_t frames,
                        uint32_t format, int32_t key);
int32_t xavi_codec_receive(XaviCodec *codec, XaviOutput **output);
int32_t xavi_codec_drain(XaviCodec *codec);
void xavi_output_free(XaviOutput *output);
