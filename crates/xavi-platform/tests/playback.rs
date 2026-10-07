use xavi_core::ErrorKind;
use xavi_platform::player::Player;

#[test]
fn playback_rejects_invalid_path_or_wrong_thread() {
    // The Rust test worker is not the application's main event-loop thread.
    // Other supported targets validate the missing input path before decoding.
    let error = match Player::open("unused-by-this-test.mp4") {
        Ok(_) => panic!("playback unexpectedly opened off the main thread"),
        Err(error) => error,
    };
    #[cfg(target_vendor = "apple")]
    assert_eq!(error.kind, ErrorKind::InvalidState);
    #[cfg(any(target_os = "linux", target_os = "windows", target_os = "android"))]
    assert_eq!(error.kind, ErrorKind::Io);
    #[cfg(not(any(
        target_vendor = "apple",
        target_os = "linux",
        target_os = "windows",
        target_os = "android"
    )))]
    assert_eq!(error.kind, ErrorKind::NotSupported);
}
