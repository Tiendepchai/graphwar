#![forbid(unsafe_code)]

pub mod geometry;
pub mod state;

#[cfg(target_arch = "wasm32")]
mod browser;

#[cfg(target_arch = "wasm32")]
pub use browser::start;
