//! A compile and execution fixture for the shared adapter and generated ABI.
//! Carriers have Rayzor's shapes but no GC; this is not a runtime plugin.

#![allow(dead_code, non_snake_case, unused_unsafe, clippy::all)]

pub mod runtime;
use runtime::{Buffer, BufferMut, Enum, ErrorKind, Future, NativeEnum, Rooted, Text, host};

thread_local! {
    static MEDIA: xavi_backend::MediaBackend = const { xavi_backend::MediaBackend::new() };
}

fn with_media<T>(f: impl FnOnce(&xavi_backend::MediaBackend) -> T) -> T {
    MEDIA.with(f)
}

pub mod backend {
    include!(concat!(env!("OUT_DIR"), "/xavi_backend/native.rs"));
}

mod rayzor_plugin {
    #[repr(C)]
    pub struct NativeMethodDesc {
        pub symbol_name: *const u8,
        pub symbol_name_len: usize,
        pub class_name: *const u8,
        pub class_name_len: usize,
        pub method_name: *const u8,
        pub method_name_len: usize,
        pub is_static: u8,
        pub param_count: u8,
        pub return_type: u8,
        pub param_types: [u8; 16],
    }
    // All descriptor pointers refer to immutable generated static strings.
    unsafe impl Sync for NativeMethodDesc {}
}

include!(concat!(env!("OUT_DIR"), "/media.rs"));

#[cfg(test)]
mod tests;
