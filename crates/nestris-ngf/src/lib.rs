//! NGF — the NestrisChamps Game Format (`.ngf` / `.ngf.gz`).
//!
//! Frame-per-record binary recordings of NES Tetris games: field state,
//! score/lines/level, preview and current piece, per-piece counts and DAS.
//! This crate provides:
//!
//! - [`codec`] — sans-io frame encoder (version 3) and decoder (versions
//!   1–3), bit-for-bit compatible with the NestrisChamps `BinaryFrame`
//!   encoding and the NestrisLTM importer.
//! - [`io`] — buffered reader/writer adapters and gzip handling
//!   (`std` + `gz` features).
//! - [`recorder`] — turns a stream of engine [`nestris_engine::output::OutputFrame`]s
//!   into per-game NGF recordings.
//! - [`replay`] — loads an NGF file and re-derives full engine output
//!   (including statistics) frame by frame.
//!
//! The byte-level format is documented in `docs/NGF.md`.

pub mod codec;
#[cfg(feature = "std")]
pub mod io;

pub use codec::{NgfError, NgfFrame, decode_frame, encode_v3};
