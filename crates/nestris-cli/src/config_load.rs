//! Engine-config loading for the CLI: TOML/JSON/YAML files (dispatched by
//! extension), named presets, and repeatable `--set path.to.field=value`
//! overrides applied on top of the file.
//!
//! The file parsing and override machinery lives in
//! `nestris_host::config_overlay` (shared with the station daemon).

use std::path::Path;

use anyhow::{Context, Result, bail};
use nestris_engine::config::EngineConfig;
use nestris_host::config_overlay;

/// Load a config file (`.toml`, `.json`, `.yaml`/`.yml`), apply the named
/// preset (if any) and `--set` overrides, in that order.
pub fn load(
    path: Option<&Path>,
    preset: Option<&str>,
    overrides: &[String],
) -> Result<EngineConfig> {
    let mut value = match path {
        Some(path) => config_overlay::file_to_value(path)?,
        None => serde_json::to_value(EngineConfig::default())?,
    };

    // Presets are applied before --set so explicit overrides always win.
    if let Some(name) = preset {
        let mut cfg: EngineConfig =
            serde_json::from_value(value).context("parse config before preset")?;
        apply_preset(&mut cfg, name)?;
        value = serde_json::to_value(cfg)?;
    }

    config_overlay::apply_overrides::<EngineConfig>(&mut value, overrides)?;
    serde_json::from_value(value).context("config did not match the engine schema")
}

fn apply_preset(cfg: &mut EngineConfig, name: &str) -> Result<()> {
    match name {
        "handheld" => cfg.apply_handheld_preset(),
        other => bail!("unknown preset {other:?} (available: handheld)"),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_temp(name: &str, contents: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join("nestris-config-tests");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(contents.as_bytes()).unwrap();
        path
    }

    #[test]
    fn defaults_without_file() {
        let cfg = load(None, None, &[]).unwrap();
        assert!(!cfg.tracking.enabled);
        assert!(!cfg.output.extended_stats);
        assert_eq!(cfg.fusion.vote_window, 5);
    }

    #[test]
    fn loads_toml() {
        let path = write_temp("t.toml", "[fusion]\nvote_window = 9\n");
        let cfg = load(Some(&path), None, &[]).unwrap();
        assert_eq!(cfg.fusion.vote_window, 9);
    }

    #[test]
    fn loads_json() {
        let path = write_temp("t.json", r#"{"fusion": {"vote_window": 7}}"#);
        let cfg = load(Some(&path), None, &[]).unwrap();
        assert_eq!(cfg.fusion.vote_window, 7);
    }

    #[test]
    fn loads_yaml() {
        let path = write_temp("t.yaml", "fusion:\n  vote_window: 11\n");
        let cfg = load(Some(&path), None, &[]).unwrap();
        assert_eq!(cfg.fusion.vote_window, 11);
    }

    #[test]
    fn rejects_unknown_extension() {
        let path = write_temp("t.ini", "vote_window=1");
        assert!(load(Some(&path), None, &[]).is_err());
    }

    #[test]
    fn set_overrides_scalars_and_strings() {
        let cfg = load(
            None,
            None,
            &[
                "fusion.vote_window=9".into(),
                "tracking.enabled=true".into(),
                "recognition.score_base=hex".into(),
                "calibration.smooth_alpha=0.25".into(),
            ],
        )
        .unwrap();
        assert_eq!(cfg.fusion.vote_window, 9);
        assert!(cfg.tracking.enabled);
        assert_eq!(cfg.recognition.score_base, "hex");
        assert_eq!(cfg.calibration.smooth_alpha, 0.25);
    }

    #[test]
    fn set_wins_over_preset_and_file() {
        let path = write_temp("t2.toml", "[tracking]\nenabled = false\n");
        let cfg = load(
            Some(&path),
            Some("handheld"),
            &["tracking.search_radius_px=12".into()],
        )
        .unwrap();
        assert!(cfg.tracking.enabled, "preset applies over the file");
        assert_eq!(cfg.tracking.search_radius_px, 12, "--set applies last");
    }

    #[test]
    fn handheld_preset_enables_tracking() {
        let cfg = load(None, Some("handheld"), &[]).unwrap();
        assert!(cfg.tracking.enabled);
    }

    #[test]
    fn bad_assignment_is_an_error() {
        assert!(load(None, None, &["fusion.vote_window".into()]).is_err());
        assert!(load(None, None, &["=5".into()]).is_err());
    }

    #[test]
    fn typo_in_field_path_is_an_error() {
        assert!(load(None, None, &["fusionn.vote_window=7".into()]).is_err());
        assert!(load(None, None, &["fusion.vote_windw=7".into()]).is_err());
    }

    #[test]
    fn type_mismatch_is_an_error() {
        assert!(load(None, None, &["fusion.vote_window=nope".into()]).is_err());
    }
}
