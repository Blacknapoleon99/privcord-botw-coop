//! PrivCord's Breath of the Wild co-op layer, extracted from the PrivCord app for showcase.
//!
//! Everything here sits *around* the BOTW Multiplayer 1.0.4 mod (whose server, DLL and game files are not part of
//! this repository and are not modified): it frames tunnel traffic, smooths the remote player, adds downed/revive,
//! generates a co-op difficulty pack from the player's own game dump, picks graphics presets, and drives the stock
//! dedicated server through its config file and console.

pub mod botw_difficulty;
pub mod botw_downed;
pub mod botw_downed_sidecar;
pub mod botw_enemies;
pub mod botw_enemies_sidecar;
pub mod botw_graphics;
pub mod botw_smooth;
pub mod botw_smooth_sidecar;
pub mod botwm;
pub mod framing;
