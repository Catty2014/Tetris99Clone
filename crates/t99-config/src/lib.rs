#![forbid(unsafe_code)]

use serde::Deserialize;
use std::fmt;
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub enum ConfigError {
    Io(String),
    Toml(String),
    Env(String),
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConfigError::Io(e) => write!(f, "config io: {e}"),
            ConfigError::Toml(e) => write!(f, "config toml: {e}"),
            ConfigError::Env(e) => write!(f, "config env: {e}"),
        }
    }
}

impl std::error::Error for ConfigError {}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct ClientConfig {
    pub client: ClientSection,
    pub control: ControlSection,
    pub display: DisplaySection,
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            client: ClientSection::default(),
            control: ControlSection::default(),
            display: DisplaySection::default(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct ClientSection {
    pub server: String,
    pub observer: bool,
    pub spectate: usize,
}

impl Default for ClientSection {
    fn default() -> Self {
        Self {
            server: "ws://127.0.0.1:6666".into(),
            observer: false,
            spectate: 0,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct ControlSection {
    pub das_ticks: u32,
    pub arr_ticks: u32,
    pub dual_charge: bool,
}

impl Default for ControlSection {
    fn default() -> Self {
        Self {
            das_ticks: 10,
            arr_ticks: 2,
            dual_charge: false,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct DisplaySection {
    pub vsync: bool,
    pub framerate: u32,
    pub opponent_framerate: u32,
    pub ghost: bool,
    pub shadow_locked: bool,
    pub result_hold_secs: u64,
}

impl Default for DisplaySection {
    fn default() -> Self {
        Self {
            vsync: false,
            framerate: 0,
            opponent_framerate: 60,
            ghost: true,
            shadow_locked: true,
            result_hold_secs: 0,
        }
    }
}

impl ClientConfig {
    pub fn from_toml(s: &str) -> Result<Self, ConfigError> {
        toml::from_str(s).map_err(|e| ConfigError::Toml(e.to_string()))
    }
}

/// 优先级：环境变量 > 文件；CLI 由调用方在拿到 Config 后覆盖。
/// `--config` 由调用方传入；否则 T99_CONFIG；否则 ./client.toml。
pub fn load_client(explicit: Option<&Path>) -> Result<ClientConfig, ConfigError> {
    let (path, required) = match explicit {
        Some(p) => (Some(p.to_path_buf()), true),
        None => match std::env::var_os("T99_CONFIG") {
            Some(p) => (Some(PathBuf::from(p)), true),
            None => (Some(PathBuf::from("client.toml")), false),
        },
    };

    let mut cfg = match path {
        Some(p) => match std::fs::read_to_string(&p) {
            Ok(s) => ClientConfig::from_toml(&s)?,
            Err(e) if !required && e.kind() == std::io::ErrorKind::NotFound => {
                ClientConfig::default()
            }
            Err(e) => return Err(ConfigError::Io(format!("{}: {e}", p.display()))),
        },
        None => ClientConfig::default(),
    };
    apply_env(&mut cfg)?;
    Ok(cfg)
}

fn env_string(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

fn env_bool(name: &str) -> Result<Option<bool>, ConfigError> {
    match env_string(name) {
        None => Ok(None),
        Some(v) => match v.to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => Ok(Some(true)),
            "0" | "false" | "no" | "off" => Ok(Some(false)),
            _ => Err(ConfigError::Env(format!("{name}={v:?} is not a bool"))),
        },
    }
}

fn env_u32(name: &str) -> Result<Option<u32>, ConfigError> {
    match env_string(name) {
        None => Ok(None),
        Some(v) => v
            .parse()
            .map(Some)
            .map_err(|_| ConfigError::Env(format!("{name}={v:?} is not u32"))),
    }
}

fn env_u64(name: &str) -> Result<Option<u64>, ConfigError> {
    match env_string(name) {
        None => Ok(None),
        Some(v) => v
            .parse()
            .map(Some)
            .map_err(|_| ConfigError::Env(format!("{name}={v:?} is not u64"))),
    }
}

fn apply_env(cfg: &mut ClientConfig) -> Result<(), ConfigError> {
    if let Some(v) = env_string("T99_CLIENT__SERVER") {
        cfg.client.server = v;
    }
    if let Some(v) = env_bool("T99_CLIENT__OBSERVER")? {
        cfg.client.observer = v;
    }
    if let Some(v) = env_u64("T99_CLIENT__SPECTATE")? {
        cfg.client.spectate = v as usize;
    }

    if let Some(v) = env_u32("T99_CONTROL__DAS_TICKS")? {
        cfg.control.das_ticks = v;
    }
    if let Some(v) = env_u32("T99_CONTROL__ARR_TICKS")? {
        cfg.control.arr_ticks = v;
    }
    if let Some(v) = env_bool("T99_CONTROL__DUAL_CHARGE")? {
        cfg.control.dual_charge = v;
    }

    if let Some(v) = env_bool("T99_DISPLAY__VSYNC")? {
        cfg.display.vsync = v;
    }
    if let Some(v) = env_u32("T99_DISPLAY__FRAMERATE")? {
        cfg.display.framerate = v;
    }
    if let Some(v) = env_u32("T99_DISPLAY__OPPONENT_FRAMERATE")? {
        cfg.display.opponent_framerate = v;
    }
    if let Some(v) = env_bool("T99_DISPLAY__GHOST")? {
        cfg.display.ghost = v;
    }
    if let Some(v) = env_bool("T99_DISPLAY__SHADOW_LOCKED")? {
        cfg.display.shadow_locked = v;
    }
    if let Some(v) = env_u64("T99_DISPLAY__RESULT_HOLD_SECS")? {
        cfg.display.result_hold_secs = v;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_client_toml() {
        let cfg = ClientConfig::from_toml(
            r#"
[client]
server = "ws://127.0.0.1:9000"
observer = true
spectate = 3

[control]
das_ticks = 10
arr_ticks = 2
dual_charge = true

[display]
vsync = false
framerate = 0
opponent_framerate = 30
ghost = false
shadow_locked = true
result_hold_secs = 5
"#,
        )
        .unwrap();
        assert_eq!(cfg.client.server, "ws://127.0.0.1:9000");
        assert!(cfg.client.observer);
        assert_eq!(cfg.client.spectate, 3);
        assert!(cfg.control.dual_charge);
        assert_eq!(cfg.display.opponent_framerate, 30);
        assert!(!cfg.display.ghost);
    }

    #[test]
    fn defaults_are_sane() {
        let cfg = ClientConfig::default();
        assert_eq!(cfg.control.das_ticks, 10);
        assert_eq!(cfg.control.arr_ticks, 2);
        assert_eq!(cfg.display.opponent_framerate, 60);
        assert!(!cfg.display.vsync);
    }
}
