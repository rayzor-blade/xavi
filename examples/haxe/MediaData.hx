import haxe.Int64;
import haxe.io.Bytes;
import media.AudioData;
import media.AudioDataInit;
import media.AudioDataCopyToOptions;
import media.AudioSampleFormat;
import media.VideoFrame;
import media.VideoFrameBufferInit;
import media.VideoFrameCopyToOptions;
import media.VideoPixelFormat;

// Generate `media` with xavi-haxe and add that directory to the classpath.
// Running this requires a host plugin built with xavi-bindgen and the shared
// adapter; xavi-check exercises the same operations without a runtime VM.
class MediaData {
    static function main() {
        var source = Bytes.alloc(3);
        source.set(0, 128);
        source.set(1, 192);
        source.set(2, 255);
        var audio = AudioData.create(new AudioDataInit(
            AudioSampleFormat.U8, 48000, Int64.ofInt(3), Int64.ofInt(1), Int64.ofInt(-9), source
        ));
        var options = new AudioDataCopyToOptions(Int64.ofInt(0));
        options.format(AudioSampleFormat.F32Planar);
        var pcm = Bytes.alloc(Int64.toInt(audio.allocationSize(options)));
        audio.copyTo(pcm, options);
        trace(audio.numberOfFrames());
        audio.close();

        var pixels = Bytes.alloc(4);
        pixels.set(3, 255);
        var frame = VideoFrame.create(pixels, new VideoFrameBufferInit(
            VideoPixelFormat.RGBA, Int64.ofInt(1), Int64.ofInt(1), Int64.ofInt(0)
        ));
        var copy = new VideoFrameCopyToOptions();
        var destination = Bytes.alloc(Int64.toInt(frame.allocationSize(copy)));
        var layout = frame.copyTo(destination, copy).await();
        trace(layout.stride(0));
        layout.close();
        frame.close();
    }
}
