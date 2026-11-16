//! InkCraft core library.
//!
//! The binary in `main.rs` wires these modules to a `winit` window. Keeping the
//! logic in a library lets the storage, spatial and smoothing code be tested
//! without a display or GPU.

pub mod document;
pub mod guides;
pub mod input;
pub mod model;
pub mod pdf;
pub mod renderer;
pub mod spatial;
pub mod storage;
pub mod ui;
