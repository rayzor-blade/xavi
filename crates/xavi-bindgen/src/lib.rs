//! Generates the native media surface from one declaration and the shared IDL.
//! Exports media data operations and native cross-platform file playback.
//! Lower-level codec sessions, capture and demuxing are not yet exposed.
//!
//! From this workspace, generate Haxe with
//! `cargo run -p xavi-bindgen --bin xavi-haxe -- ash <output>` (or `rayzor`).
//! An adapter build script calls [`generate`] and `xavi_backend::install`, then
//! includes both outputs. The generator is the sibling `../x-idl` checkout;
//! generated code is a build artifact, not checked into this repository.
//! See `examples/haxe/MediaData.hx` for the native data API. Running it requires
//! a VM plugin that supplies runtime carriers and the backend context hook.

pub use x_idl::haxe::{self};

pub const MEDIA_API: &str = include_str!("../../../api/media.api.rs");
pub const MEDIA_IDL: &str = include_str!("../../../api/spec/media.idl");
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
