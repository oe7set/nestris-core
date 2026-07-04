//! GUI-agnostic desktop core shared by the frontends (egui, Qt): the
//! pipeline worker, persisted settings, the per-game session store, and
//! small helpers (event formatting, game-end tracking) that must behave
//! identically in every GUI.

pub mod events;
pub mod game_end;
pub mod session;
pub mod settings;
pub mod worker;

/// Per-user config directory: `%APPDATA%\nestris-core` on Windows, the
/// platform config dir elsewhere; falls back to the working directory.
pub fn config_dir() -> std::path::PathBuf {
    #[cfg(windows)]
    let base = std::env::var("APPDATA").ok().map(std::path::PathBuf::from);
    #[cfg(not(windows))]
    let base = dirs::config_dir();
    base.unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("nestris-core")
}
