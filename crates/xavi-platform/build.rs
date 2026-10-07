fn main() {
    println!("cargo:rerun-if-changed=src/apple.c");
    println!("cargo:rerun-if-changed=src/native.h");
    println!("cargo:rerun-if-changed=src/mux/apple.m");
    println!("cargo:rerun-if-changed=src/player/apple.m");
    let vendor = std::env::var("CARGO_CFG_TARGET_VENDOR").unwrap();
    if vendor == "apple" {
        cc::Build::new()
            .file("src/apple.c")
            .flag("-std=c11")
            .warnings_into_errors(true)
            .compile("xavi_apple");
        cc::Build::new()
            .file("src/mux/apple.m")
            .flag("-fobjc-arc")
            .flag("-fblocks")
            .warnings_into_errors(true)
            .compile("xavi_apple_mux");
        cc::Build::new()
            .file("src/player/apple.m")
            .flag("-fobjc-arc")
            .flag("-fblocks")
            .warnings_into_errors(true)
            .compile("xavi_apple_player");
        for framework in [
            "AudioToolbox",
            "VideoToolbox",
            "CoreMedia",
            "CoreVideo",
            "CoreFoundation",
            "AVFoundation",
            "Foundation",
        ] {
            println!("cargo:rustc-link-lib=framework={framework}");
        }
    }
}
