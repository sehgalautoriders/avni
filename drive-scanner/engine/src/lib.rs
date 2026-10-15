//! AVNI Drive Scanner engine. No UI here: the Tauri app (app/src-tauri) and the `avni-scan` CLI both call this crate.
//!
//! Pipeline: drives -> metadata (NTFS MFT, or folder walk) -> rules (zones) -> duplicates (size -> sampled BLAKE3 ->
//! full BLAKE3, in on-disk order) -> rules again (duplicates, already-on-Drive) -> summary / queries / reports / actions.

pub mod actions;
pub mod drives;
pub mod extract;
pub mod hash;
pub mod index;
pub mod model;
pub mod ntfs;
pub mod platform;
pub mod report;
pub mod rules;
pub mod scan;
pub mod settings;
pub mod supabase;
pub mod treelaw;
pub mod util;
pub mod viewer;
pub mod walk;
