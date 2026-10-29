//! LightCraft Mobile: a touch-first, minimal photo editor built on the LightCraft develop
//! pipeline, compiled to WASM and shipped in the Android app (`cargo xtask mobile`).
//!
//! Everything but the host glue is plain Rust that builds and tests natively:
//! - [`photo`]: decode, preview renders, full-resolution JPEG export;
//! - [`analysis`]: deterministic photo analysis (tonal and colour statistics, Shannon-entropy
//!   saliency that locates the subject);
//! - [`looks`]: adaptive looks whose settings are computed from each photo's analysis;
//! - [`advisor`]: the Auto assistant (an enhancement plus plain-language guidance);
//! - [`recipe`]: the editable, serializable description of an edit and its undo history;
//! - [`library`]: which photos and saved versions the app keeps;
//! - [`ui`]: the egui interface.
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

pub mod advisor;
pub mod analysis;
pub mod library;
pub mod looks;
pub mod photo;
pub mod recipe;
pub mod ui;
