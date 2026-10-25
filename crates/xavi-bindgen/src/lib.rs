//! Generates the native media surface from one declaration and the shared IDL.
//! Exports media data operations, editing, native cross-platform file playback,
//! bounded codec streams, and MP4 muxing and demuxing.
//!
//! From this workspace, generate Haxe with
//! `cargo run -p xavi-bindgen --bin xavi-haxe -- ash <output>` (or `rayzor`).
//! An adapter build script calls [`generate`] and `xavi_backend::install`, then
//! includes both outputs. The generator is x-idl at the revision the workspace
//! pins; generated code is a build artifact, not checked into this repository.
//! See `examples/haxe/MediaData.hx` for the native data API. Running it requires
//! a VM plugin that supplies runtime carriers and the backend context hook.

pub use x_idl::haxe::{self};

pub const MEDIA_API: &str = include_str!("../../../api/media.api.rs");
pub const MEDIA_IDL: &str = include_str!("../../../api/spec/media.idl");
/// The page agent's commands, for a program in a browser.
pub const AGENT_IDL: &str = include_str!("../../../api/spec/agent.idl");
pub const NAMESPACE: &str = "media";
pub const LIBRARY: x_idl::Library<'static> = x_idl::Library("xavi");

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Runtime {
    Caribou,
    HashLink,
    Rayzor,
}

/// Include the generated model beside a `backend` module installed by
/// `xavi_backend::install`. Runtime carriers live at the model's crate root.
/// HashLink also needs its managed-record helpers under `crate::runtime`,
/// `hl_abi` and `ash_future_abi`; Caribou needs `caribou_abi`; Rayzor needs
/// `rayzor_plugin` for the generated method descriptors. Adapters provide
/// `with_media` as documented by the backend installer.
pub fn generate(runtime: Runtime) -> Result<String, String> {
    match runtime {
        Runtime::Caribou => x_idl::generate_caribou(NAMESPACE, MEDIA_API, MEDIA_IDL),
        Runtime::HashLink => LIBRARY.generate_hashlink(NAMESPACE, MEDIA_API, MEDIA_IDL),
        Runtime::Rayzor => LIBRARY.generate_rayzor(NAMESPACE, MEDIA_API, MEDIA_IDL, &[]),
    }
}

pub fn haxe(runtime: haxe::Runtime) -> Result<Vec<haxe::File>, String> {
    LIBRARY.haxe(NAMESPACE, MEDIA_API, MEDIA_IDL, runtime)
}

/// The browser wire's schema: the media types the model names, and the agent's commands.
pub fn browser_idl() -> String {
    format!("{MEDIA_IDL}\n{AGENT_IDL}")
}

/// The browser wire: Rust for `crate::wire`, and the JavaScript a page serves as
/// `xavi_wire.mjs` beside the agent.
pub fn browser_wire() -> Result<x_idl::wire::Wire, String> {
    x_idl::wire::wire(&browser_idl())
}

/// The `backend` module for a browser build: each function `implemented`
/// (xavi-backend's `WEB`) defines forwards to `crate::web`, and any other raises
/// that it is not available in a page.
pub fn web_backend(runtime: Runtime, implemented: &str) -> Result<String, String> {
    let idl = browser_idl();
    match runtime {
        Runtime::Caribou => x_idl::web_backend(NAMESPACE, MEDIA_API, &idl, implemented),
        Runtime::HashLink | Runtime::Rayzor => {
            x_idl::hashlink_web_backend(NAMESPACE, MEDIA_API, &idl, implemented)
        }
    }
}
