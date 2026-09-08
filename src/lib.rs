//! Shared engine code, usable without a graphics context.

mod portable_path;
#[cfg(feature = "assets")]
mod rooted_path;

pub mod assets;
pub mod bundle;
pub mod collision;
pub mod drawing;
pub mod input;
pub mod kernel;
pub mod manifest;
pub mod maps;
pub mod tilemap;

#[cfg(feature = "native-plugins")]
pub mod plugins;

#[cfg(feature = "graphics")]
pub mod rendering;

#[cfg(feature = "scripting")]
pub mod scripting;

#[cfg(feature = "scripting")]
pub mod runtime;
