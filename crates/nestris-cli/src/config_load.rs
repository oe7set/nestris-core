//! Engine-config loading for the CLI: TOML/JSON/YAML files (dispatched by
//! extension), named presets, and repeatable `--set path.to.field=value`
//! overrides applied on top of the file.
//!
//! Overrides work on a `serde_json::Value` tree so they compose with any
//! config format: file → Value, dotted-path assignments, then a strict
//! deserialization into [`EngineConfig`] which rejects unknown paths.

use std::path::Path;

use anyhow::{Context, Result, bail};
use nestris_engine::config::EngineConfig;
use serde_json::Value;

/// Load a config file (`.toml`, `.json`, `.yaml`/`.yml`), apply the named
/// preset (if any) and `--set` overrides, in that order.
pub fn load(
    path: Option<&Path>,
    preset: Option<&str>,
    overrides: &[String],
) -> Result<EngineConfig> {
    let mut value = match path {
        Some(path) => file_to_value(path)?,
        None => serde_json::to_value(EngineConfig::default())?,
    };

    // Presets are applied before --set so explicit overrides always win.
    if let Some(name) = preset {
        let mut cfg: EngineConfig =
            serde_json::from_value(value).context("parse config before preset")?;
        apply_preset(&mut cfg, name)?;
        value = serde_json::to_value(cfg)?;
    }

    for assignment in overrides {
        apply_override(&mut value, assignment)?;
    }

    serde_json::from_value(value).context("config did not match the engine schema")
}

fn file_to_value(path: &Path) -> Result<Value> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("read config {}", path.display()))?;
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default();
    let value = match ext.as_str() {
        "toml" => {
            let cfg: toml::Value = toml::from_str(&raw).context("parse config TOML")?;
            serde_json::to_value(cfg)?
        }
        "json" => serde_json::from_str(&raw).context("parse config JSON")?,
        "yaml" | "yml" => serde_yaml_ng::from_str(&raw).context("parse config YAML")?,
        other => bail!(
            "unsupported config extension {other:?} (expected .toml, .json, .yaml or .yml)"
        ),
    };
    Ok(value)
}

fn apply_preset(cfg: &mut EngineConfig, name: &str) -> Result<()> {
    match name {
        "handheld" => cfg.apply_handheld_preset(),
        other => bail!("unknown preset {other:?} (available: handheld)"),
    }
    Ok(())
}

/// Apply one `path.to.field=value` assignment. The right-hand side is parsed
/// as JSON when possible (numbers, booleans, quoted strings) and falls back
/// to a plain string, so `--set recognition.score_base=hex` works unquoted.
///
/// Paths are validated against the schema (the serialized default config):
/// `EngineConfig` tolerates unknown keys when parsing, so without this check
/// a typo like `--set fusionn.vote_window=7` would be silently ignored.
fn apply_override(root: &mut Value, assignment: &str) -> Result<()> {
    let Some((path, raw_value)) = assignment.split_once('=') else {
        bail!("--set expects path.to.field=value, got {assignment:?}");
    };
    let path = path.trim();
    if path.is_empty() {
        bail!("--set has an empty field path in {assignment:?}");
    }
    validate_path(path)?;
    let value: Value = serde_json::from_str(raw_value.trim())
        .unwrap_or_else(|_| Value::String(raw_value.trim().to_string()));

    let mut node = root;
    let segments: Vec<&str> = path.split('.').collect();
    for (i, segment) in segments.iter().enumerate() {
        let map = node
            .as_object_mut()
            .with_context(|| format!("config path {path:?}: {segment:?} is not a section"))?;
        if i == segments.len() - 1 {
            map.insert((*segment).to_string(), value);
            return Ok(());
        }
        node = map
            .entry((*segment).to_string())
            .or_insert_with(|| Value::Object(Default::default()));
    }
    unreachable!("segments is never empty");
}

/// Ensure every segment of `path` exists in the engine-config schema.
fn validate_path(path: &str) -> Result<()> {
    let schema = serde_json::to_value(EngineConfig::default()).expect("default serializes");
    let mut node = &schema;
    for segment in path.split('.') {
        match node.get(segment) {
            Some(next) => node = next,
            None => {
                let known = node
                    .as_object()
                    .map(|m| m.keys().cloned().collect::<Vec<_>>().join(", "))
                    .unwrap_or_default();
                bail!("unknown config field {segment:?} in {path:?} (known: {known})");
            }
        }
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
