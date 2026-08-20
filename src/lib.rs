//! unburn — a display uniformity compensator.
//!
//! The program paints a transparent, non-interactive layer over a monitor whose
//! alpha varies across the screen, so that a panel with smooth bright or tinted
//! patches reads as uniform. It never captures or re-renders the desktop; the
//! compositor does the blending.

pub mod app;
pub mod cli;
pub mod compensation;
pub mod config;
pub mod display;
pub mod gui;
pub mod ipc;
pub mod overlay;
pub mod platform;

pub use compensation::{Defect, Mask, MaskParams, MaskQuality, RadialDefect, Rgb, Vec2};
pub use config::{Config, DisplayConfig};
pub use display::{DisplayIdentity, OutputId, OutputInfo, OverlayId};
