use xavi_bindgen::{Runtime, generate, haxe};

#[test]
fn every_runtime_generates_the_implemented_surface() {
    for runtime in [Runtime::Caribou, Runtime::HashLink, Runtime::Rayzor] {
        let source = generate(runtime).unwrap();
        syn::parse_file(&source).unwrap();
        for name in [
            "AudioData",
            "AudioEqualizer",
            "VideoFrame",
            "EncodedAudioChunk",
            "EncodedVideoChunk",
            "PlaneLayouts",
            "MediaPlayer",
            "CodecConfiguration",
            "MediaQueue",
            "AudioEncoder",
            "VideoEncoder",
            "AudioDecoder",
            "VideoDecoder",
            "MediaMuxer",
            "MediaDemuxer",
        ] {
            assert!(
                source.contains(&format!("struct {name} ")),
                "{runtime:?}: {name}"
            );
        }
        assert!(source.contains("Future < PlaneLayouts >"));
    }
}

#[test]
fn haxe_uses_typed_buffers_counts_and_copy_completion() {
    for runtime in [haxe::Runtime::HashLink, haxe::Runtime::Rayzor] {
        let files = xavi_bindgen::haxe(runtime).unwrap();
        let file = |name: &str| {
            &files
                .iter()
                .find(|file| file.path == format!("media/{name}.hx"))
                .unwrap()
                .source
        };
        let audio = file("AudioDataInit");
        assert!(audio.contains("numberOfFrames:haxe.Int64"));
        assert!(audio.contains("data:haxe.io.Bytes"));
        assert!(file("VideoFrame").contains("Future<PlaneLayouts>"));
        assert!(file("OptionalDuration").contains("Unknown"));
        assert!(file("OptionalDuration").contains("Value(microseconds:haxe.Int64)"));
        assert!(file("PlaneLayouts").contains("close():Void"));
        assert!(file("MediaPlayer").contains("pollFrame():Bool"));
        assert!(file("MediaPlayer").contains("takeFrame():VideoFrame"));
        assert!(file("MediaPlayer").contains("seek(seconds:Float):Void"));
        assert!(file("AudioEqualizer").contains("process(data:AudioData):AudioData"));
        assert!(file("MediaPlayer").contains("setEqualizer(equalizer:AudioEqualizer):Void"));
    }
}
