#![forbid(unsafe_code)]

pub mod animation;
pub mod geometry;
pub mod preview;
pub mod state;

#[cfg(all(test, not(target_arch = "wasm32")))]
#[path = "canvas/vfx.rs"]
mod vfx;

#[cfg(target_arch = "wasm32")]
pub mod audio;
#[cfg(target_arch = "wasm32")]
mod browser;
#[cfg(target_arch = "wasm32")]
pub mod canvas;

#[cfg(target_arch = "wasm32")]
pub use browser::start;
