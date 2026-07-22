#![forbid(unsafe_code)]

pub mod animation;
pub mod geometry;
pub mod preview;
pub mod state;

#[cfg(target_arch = "wasm32")]
mod browser;

#[cfg(target_arch = "wasm32")]
pub use browser::start;
