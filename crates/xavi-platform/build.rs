fn main() {
    println!("cargo:rerun-if-changed=src/apple.c");
    println!("cargo:rerun-if-changed=src/native.h");
    let vendor = std::env::var("CARGO_CFG_TARGET_VENDOR").unwrap();
    if vendor == "apple" {
        cc::Build::new()
            .file("src/apple.c")
            .flag("-std=c11")
            .warnings_into_errors(true)
            .compile("xavi_apple");
        for framework in [
            "AudioToolbox",
            "VideoToolbox",
            "CoreMedia",
            "CoreVideo",
            "CoreFoundation",
        ] {
            println!("cargo:rustc-link-lib=framework={framework}");
        }
    }
}
