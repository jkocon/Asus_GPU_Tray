//! Asus GPU Tray, Rust rewrite. See docs/rust-rewrite-plan.md in the repository root.
//!
//! `gpu` is the shared core without any UI: detection, state, labels, processes. It mirrors the
//! pure functions of asus_gpu_tray.py one to one, so the Python tests double as its spec.

pub mod gpu;
pub mod helpers;
pub mod tray;
