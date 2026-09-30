//! Layered config loading shared by the native hosts: a TOML/JSON/YAML file
//! (dispatched by extension), then dotted-path `path.to.field=value`
//! overrides (`--set`, environment), then a strict deserialization.
//!
//! Overrides work on a `serde_json::Value` tree so they compose with any
//! config format. Every override path is validated against the serialized
//! default config, because the config structs tolerate unknown keys when
//! parsing and a typo would otherwise be silently ignored.

use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

/// Load `T` from an optional file plus `overrides` (applied in order).
pub fn load<T: Serialize + DeserializeOwned + Default>(
    path: Option<&Path>,
    overrides: &[String],
) -> Result<T> {
    let mut value = match path {
        Some(path) => file_to_value(path)?,
        None => serde_json::to_value(T::default())?,
    };
    apply_overrides::<T>(&mut value, overrides)?;
    serde_json::from_value(value).context("config did not match the schema")
}

/// Parse a config file into a JSON value tree by its extension.
pub fn file_to_value(path: &Path) -> Result<Value> {
    let raw =
        std::fs::read_to_string(path).with_context(|| format!("read config {}", path.display()))?;
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
        other => {
            bail!("unsupported config extension {other:?} (expected .toml, .json, .yaml or .yml)")
        }
    };
    Ok(value)
}

/// Apply `path=value` assignments to `root`, validated against `T`'s schema.
pub fn apply_overrides<T: Serialize + Default>(
    root: &mut Value,
    overrides: &[String],
) -> Result<()> {
    if overrides.is_empty() {
        return Ok(());
    }
    let schema = serde_json::to_value(T::default())?;
    for assignment in overrides {
        apply_override(&schema, root, assignment)?;
    }
    Ok(())
}

/// Environment overrides: `<PREFIX>__SECTION__FIELD=value` becomes
/// `section.field=value` (`__` separates path segments, names lowercased).
pub fn env_overrides(prefix: &str) -> Vec<String> {
    let lead = format!("{prefix}__");
    let mut out: Vec<String> = std::env::vars()
        .filter_map(|(key, value)| {
            let rest = key.strip_prefix(&lead)?;
            if rest.is_empty() {
                return None;
            }
            let path = rest
                .split("__")
                .map(|s| s.to_ascii_lowercase())
                .collect::<Vec<_>>()
                .join(".");
            Some(format!("{path}={value}"))
        })
        .collect();
    // Deterministic application order regardless of environment ordering.
    out.sort();
    out
}

/// Apply one `path.to.field=value` assignment. The right-hand side is parsed
/// as JSON when possible (numbers, booleans, quoted strings) and falls back
/// to a plain string, so `--set recognition.score_base=hex` works unquoted.
/// String fields always take the raw text, so a numeric-looking password or
/// id (`mqtt.password=1234`) stays a string.
fn apply_override(schema: &Value, root: &mut Value, assignment: &str) -> Result<()> {
    let Some((path, raw_value)) = assignment.split_once('=') else {
        bail!("override expects path.to.field=value, got {assignment:?}");
    };
    let path = path.trim();
    if path.is_empty() {
        bail!("override has an empty field path in {assignment:?}");
    }
    let leaf = validate_path(schema, path)?;
    let raw = raw_value.trim();
    let value: Value = match (leaf, serde_json::from_str::<Value>(raw)) {
        (Value::String(_), Ok(Value::String(s))) => Value::String(s),
        (Value::String(_), _) => Value::String(raw.to_string()),
        (_, Ok(parsed)) => parsed,
        (_, Err(_)) => Value::String(raw.to_string()),
    };

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

/// Ensure every segment of `path` exists in the schema; returns the leaf.
fn validate_path<'a>(schema: &'a Value, path: &str) -> Result<&'a Value> {
    let mut node = schema;
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
    Ok(node)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Debug, Default, Serialize, Deserialize, PartialEq)]
    #[serde(default)]
    struct Inner {
        port: u16,
        host: String,
    }

    #[derive(Debug, Default, Serialize, Deserialize, PartialEq)]
    #[serde(default)]
    struct Outer {
        mqtt: Inner,
        enabled: bool,
    }

    #[test]
    fn overrides_apply_and_validate() {
        let cfg: Outer = load(
            None,
            &["mqtt.port=1884".into(), "mqtt.host=broker.local".into()],
        )
        .unwrap();
        assert_eq!(cfg.mqtt.port, 1884);
        assert_eq!(cfg.mqtt.host, "broker.local");
        assert!(load::<Outer>(None, &["mqtt.prot=1".into()]).is_err());
        assert!(load::<Outer>(None, &["mqtt.port=nope".into()]).is_err());
    }

    #[test]
    fn numeric_text_stays_a_string_for_string_fields() {
        let cfg: Outer = load(None, &["mqtt.host=1234".into()]).unwrap();
        assert_eq!(cfg.mqtt.host, "1234");
        let cfg: Outer = load(None, &["mqtt.host=\"quoted\"".into()]).unwrap();
        assert_eq!(cfg.mqtt.host, "quoted");
    }

    #[test]
    fn env_names_map_to_paths() {
        // SAFETY: test-local variable name, no other thread reads it.
        unsafe { std::env::set_var("OVERLAYTEST__MQTT__HOST", "10.0.0.2") };
        let found = env_overrides("OVERLAYTEST");
        assert_eq!(found, vec!["mqtt.host=10.0.0.2".to_string()]);
        let cfg: Outer = load(None, &found).unwrap();
        assert_eq!(cfg.mqtt.host, "10.0.0.2");
        unsafe { std::env::remove_var("OVERLAYTEST__MQTT__HOST") };
    }
}
