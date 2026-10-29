// Code in this module PARTIALLY or FULLY utilized AI Coding Agent

#![forbid(unsafe_code)]

use serde::Deserialize;
use std::fmt;
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub enum ConfigError {
    Io(String),
    Toml(String),
    Env(String),
    Rules(String),
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConfigError::Io(e) => write!(f, "config io: {e}"),
            ConfigError::Toml(e) => write!(f, "config toml: {e}"),
            ConfigError::Env(e) => write!(f, "config env: {e}"),
            ConfigError::Rules(e) => write!(f, "config rules: {e}"),
        }
    }
}

impl std::error::Error for ConfigError {}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct ClientConfig {
    pub ruleset_file: String,
    pub client: ClientSection,
    pub control: ControlSection,
    pub display: DisplaySection,
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            ruleset_file: "ruleset.toml".into(),
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
    pub left: String,
    pub right: String,
    pub rotate: Vec<String>,
    #[serde(rename = "rotateC")]
    pub rotate_c: Vec<String>,
    pub softdrop: String,
    pub harddrop: String,
    pub hold: Vec<String>,
    pub attack: String,
    pub manual: String,
    pub attack_fallback: String,
}

impl Default for ControlSection {
    fn default() -> Self {
        Self {
            das_ticks: 10,
            arr_ticks: 2,
            dual_charge: false,
            left: "A".into(),
            right: "D".into(),
            rotate: vec!["W".into(), "X".into()],
            rotate_c: vec!["<L-C>".into(), "Z".into()],
            softdrop: "S".into(),
            harddrop: "<SP>".into(),
            hold: vec!["C".into(), "<L-S>".into()],
            attack: "<TAB>".into(),
            manual: "arrow".into(),
            attack_fallback: "random".into(),
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

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct ServerConfig {
    pub ruleset_file: String,
    pub server: ServerSection,
    pub auth: AuthSection,
    #[serde(rename = "match")]
    pub match_section: MatchSection,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            ruleset_file: "ruleset.toml".into(),
            server: ServerSection::default(),
            auth: AuthSection::default(),
            match_section: MatchSection::default(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct ServerSection {
    pub address: String,
    pub port: u16,
    pub loglevel: String,
    pub debug: bool,
    pub stalemate_accel: bool,
    pub lobby_timeout_secs: u64,
    pub restart_timeout_secs: u64,
    pub write_timeout_secs: u64,
}

impl Default for ServerSection {
    fn default() -> Self {
        Self {
            address: "127.0.0.1".into(),
            port: 6666,
            loglevel: "INFO".into(),
            debug: false,
            stalemate_accel: false,
            lobby_timeout_secs: 10,
            restart_timeout_secs: 8,
            write_timeout_secs: 5,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct AuthSection {
    pub check_player_id: bool,
    pub case_sensitive: bool,
    pub id_files: Vec<String>,
}

impl Default for AuthSection {
    fn default() -> Self {
        Self {
            check_player_id: true,
            case_sensitive: true,
            id_files: vec!["auth/players.txt".into()],
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct MatchSection {
    pub players: usize,
    pub lobby: bool,
    pub seed: u64,
}

impl Default for MatchSection {
    fn default() -> Self {
        Self {
            players: 99,
            lobby: true,
            seed: 0,
        }
    }
}

impl ServerConfig {
    pub fn from_toml(s: &str) -> Result<Self, ConfigError> {
        toml::from_str(s).map_err(|e| ConfigError::Toml(e.to_string()))
    }
}

/// 优先级：环境变量 > 文件；CLI 由调用方在拿到 Config 后覆盖。
pub fn load_client(explicit: Option<&Path>) -> Result<ClientConfig, ConfigError> {
    let (path, required) = path_for(explicit, "client.toml");
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
    apply_client_env(&mut cfg)?;
    Ok(cfg)
}

pub fn load_server(explicit: Option<&Path>) -> Result<ServerConfig, ConfigError> {
    let (path, required) = path_for(explicit, "server.toml");
    let mut cfg = match path {
        Some(p) => match std::fs::read_to_string(&p) {
            Ok(s) => ServerConfig::from_toml(&s)?,
            Err(e) if !required && e.kind() == std::io::ErrorKind::NotFound => {
                ServerConfig::default()
            }
            Err(e) => return Err(ConfigError::Io(format!("{}: {e}", p.display()))),
        },
        None => ServerConfig::default(),
    };
    apply_server_env(&mut cfg)?;
    Ok(cfg)
}

fn path_for(explicit: Option<&Path>, default: &str) -> (Option<PathBuf>, bool) {
    match explicit {
        Some(p) => (Some(p.to_path_buf()), true),
        None => match std::env::var_os("T99_CONFIG") {
            Some(p) => (Some(PathBuf::from(p)), true),
            None => (Some(PathBuf::from(default)), false),
        },
    }
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

fn apply_client_env(cfg: &mut ClientConfig) -> Result<(), ConfigError> {
    if let Some(v) = env_string("T99_RULESET_FILE") {
        cfg.ruleset_file = v;
    }
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

fn apply_server_env(cfg: &mut ServerConfig) -> Result<(), ConfigError> {
    if let Some(v) = env_string("T99_RULESET_FILE") {
        cfg.ruleset_file = v;
    }
    if let Some(v) = env_string("T99_SERVER__ADDRESS") {
        cfg.server.address = v;
    }
    if let Some(v) = env_u32("T99_SERVER__PORT")? {
        cfg.server.port = v as u16;
    }
    if let Some(v) = env_string("T99_SERVER__LOGLEVEL") {
        cfg.server.loglevel = v;
    }
    if let Some(v) = env_bool("T99_SERVER__DEBUG")? {
        cfg.server.debug = v;
    }
    if let Some(v) = env_bool("T99_SERVER__STALEMATE_ACCEL")? {
        cfg.server.stalemate_accel = v;
    }
    if let Some(v) = env_u64("T99_SERVER__LOBBY_TIMEOUT_SECS")? {
        cfg.server.lobby_timeout_secs = v;
    }
    if let Some(v) = env_u64("T99_SERVER__RESTART_TIMEOUT_SECS")? {
        cfg.server.restart_timeout_secs = v;
    }
    if let Some(v) = env_u64("T99_SERVER__WRITE_TIMEOUT_SECS")? {
        cfg.server.write_timeout_secs = v;
    }
    if let Some(v) = env_bool("T99_AUTH__CHECK_PLAYER_ID")? {
        cfg.auth.check_player_id = v;
    }
    if let Some(v) = env_bool("T99_AUTH__CASE_SENSITIVE")? {
        cfg.auth.case_sensitive = v;
    }
    if let Some(v) = env_string("T99_AUTH__ID_FILES") {
        cfg.auth.id_files = v
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
    }
    if let Some(v) = env_u32("T99_MATCH__PLAYERS")? {
        cfg.match_section.players = v as usize;
    }
    if let Some(v) = env_bool("T99_MATCH__LOBBY")? {
        cfg.match_section.lobby = v;
    }
    if let Some(v) = env_u64("T99_MATCH__SEED")? {
        cfg.match_section.seed = v;
    }
    Ok(())
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct RulesetConfig {
    pub ruleset: RulesetSection,
}

impl Default for RulesetConfig {
    fn default() -> Self {
        Self {
            ruleset: RulesetSection::default(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct RulesetSection {
    pub id: String,
    pub board_width: usize,
    pub board_height: usize,
    pub hidden_rows: usize,
    pub preview: usize,
    pub match_seconds: u64,
    pub bag_randomizer: bool,
    pub srs: bool,
    pub hold: bool,
    pub irs: bool,
    pub ihs: bool,
    pub initial_trigger: String,
    pub allow_rot180: bool,
    pub death: String,
    pub soft_milli_per_sec: i64,
    pub lock_delay: String,
    pub lock_delay_ticks: u32,
    pub lock_resets_max: u32,
    pub pending_cap_rows: u32,
    pub send_cap_rows: u32,
    pub p_same_hole_pct: u32,
    pub spawn_y_milli: i64,
    pub combo_bonus: Vec<u32>,
    pub target_bonus: Vec<u32>,
    pub badge_tier_units: Vec<u32>,
    pub stage: Vec<StageSection>,
}

impl Default for RulesetSection {
    fn default() -> Self {
        Self {
            id: "9f3c1a7e".into(),
            board_width: 10,
            board_height: 20,
            hidden_rows: 4,
            preview: 6,
            match_seconds: 900,
            bag_randomizer: true,
            srs: true,
            hold: true,
            irs: true,
            ihs: true,
            initial_trigger: "either".into(),
            allow_rot180: false,
            death: "block".into(),
            soft_milli_per_sec: 60_000,
            lock_delay: "move".into(),
            lock_delay_ticks: 30,
            lock_resets_max: 15,
            pending_cap_rows: 12,
            send_cap_rows: 20,
            p_same_hole_pct: 70,
            spawn_y_milli: 4000,
            combo_bonus: vec![0, 1, 1, 2, 2, 3, 3, 4, 4, 4, 5],
            target_bonus: vec![0, 0, 1, 3, 5, 7, 9],
            badge_tier_units: vec![2, 6, 14, 30],
            stage: default_stages(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct StageSection {
    pub ratio_min_pm: u32,
    pub gravity_base: i64,
    pub gravity_ramp_per_min: i64,
    pub gravity_cap: i64,
    pub clear_delay_ticks: u32,
    pub are_ticks: u32,
    pub garbage_stage_ticks: u32,
}

impl Default for StageSection {
    fn default() -> Self {
        Self {
            ratio_min_pm: 0,
            gravity_base: 750,
            gravity_ramp_per_min: 120,
            gravity_cap: 1125,
            clear_delay_ticks: 18,
            are_ticks: 6,
            garbage_stage_ticks: 72,
        }
    }
}

fn default_stages() -> Vec<StageSection> {
    vec![
        StageSection { ratio_min_pm: 500, gravity_base: 750, gravity_ramp_per_min: 120, gravity_cap: 1125, clear_delay_ticks: 18, are_ticks: 6, garbage_stage_ticks: 72 },
        StageSection { ratio_min_pm: 300, gravity_base: 1500, gravity_ramp_per_min: 400, gravity_cap: 2250, clear_delay_ticks: 15, are_ticks: 6, garbage_stage_ticks: 54 },
        StageSection { ratio_min_pm: 100, gravity_base: 3000, gravity_ramp_per_min: 1000, gravity_cap: 4500, clear_delay_ticks: 12, are_ticks: 5, garbage_stage_ticks: 39 },
        StageSection { ratio_min_pm: 0, gravity_base: 6000, gravity_ramp_per_min: 2000, gravity_cap: 60_000, clear_delay_ticks: 9, are_ticks: 4, garbage_stage_ticks: 27 },
    ]
}

impl RulesetConfig {
    pub fn from_toml(s: &str) -> Result<Self, ConfigError> {
        toml::from_str(s).map_err(|e| ConfigError::Toml(e.to_string()))
    }

    pub fn to_ruleset(&self) -> Result<t99_core::Ruleset, ConfigError> {
        let r = &self.ruleset;
        if r.board_width != t99_core::W || r.board_height != t99_core::H {
            return Err(ConfigError::Rules(format!(
                "board must be {}x{}, got {}x{}",
                t99_core::W, t99_core::H, r.board_width, r.board_height
            )));
        }
        if r.hidden_rows != t99_core::HIDDEN {
            return Err(ConfigError::Rules(format!(
                "hidden_rows must be {} (engine constant), got {}",
                t99_core::HIDDEN, r.hidden_rows
            )));
        }
        if !r.bag_randomizer || !r.srs || !r.hold {
            return Err(ConfigError::Rules("bag/srs/hold must be true in this engine".into()));
        }
        if r.death != "block" {
            return Err(ConfigError::Rules("death must be block".into()));
        }
        if r.lock_delay != "move" {
            return Err(ConfigError::Rules("lock_delay must be move".into()));
        }
        let trigger = match r.initial_trigger.as_str() {
            "edge" => t99_core::InitialTrigger::Edge,
            "held" => t99_core::InitialTrigger::Held,
            "either" => t99_core::InitialTrigger::Either,
            other => return Err(ConfigError::Rules(format!("initial_trigger={other:?}"))),
        };
        let tick = |n: u32| n as t99_core::Ticks * t99_core::FRAME_TICKS;
        let phases: Vec<t99_core::PhaseSpec> = r
            .stage
            .iter()
            .map(|s| t99_core::PhaseSpec {
                ratio_min_pm: s.ratio_min_pm,
                gravity_base: s.gravity_base,
                gravity_ramp_per_min: s.gravity_ramp_per_min,
                gravity_cap: s.gravity_cap,
                clear_delay: tick(s.clear_delay_ticks),
                are: tick(s.are_ticks),
                garbage_stage: tick(s.garbage_stage_ticks),
            })
            .collect();
        if phases.is_empty() {
            return Err(ConfigError::Rules("stage table is empty".into()));
        }
        Ok(t99_core::Ruleset {
            preview: r.preview,
            irs: r.irs,
            ihs: r.ihs,
            initial_trigger: trigger,
            lock_ticks: tick(r.lock_delay_ticks),
            lock_resets_max: r.lock_resets_max,
            soft_milli_per_sec: r.soft_milli_per_sec,
            phases,
            pending_cap_rows: r.pending_cap_rows,
            send_cap_rows: r.send_cap_rows,
            combo_bonus: r.combo_bonus.clone().try_into().unwrap_or([0; 11]),
            target_bonus: r.target_bonus.clone().try_into().unwrap_or([0; 7]),
            badge_tier_units: r.badge_tier_units.clone().try_into().unwrap_or([0; 4]),
            p_same_hole_pct: r.p_same_hole_pct,
            match_ticks: r.match_seconds as t99_core::Ticks * 1000 * t99_core::TMS,
            stall_accel: false,
            allow_rot180: r.allow_rot180,
            spawn_y_milli: r.spawn_y_milli,
        })
    }
}

pub fn load_ruleset(explicit: Option<&Path>) -> Result<RulesetConfig, ConfigError> {
    let path = explicit
        .map(|p| p.to_path_buf())
        .or_else(|| std::env::var_os("T99_RULESET").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("ruleset.toml"));
    let text = std::fs::read_to_string(&path)
        .map_err(|e| ConfigError::Io(format!("{}: {e}", path.display())))?;
    RulesetConfig::from_toml(&text)
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
left = "A"

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
        assert!(cfg.control.dual_charge);
        assert_eq!(cfg.display.opponent_framerate, 30);
        assert!(!cfg.display.ghost);
    }

    #[test]
    fn parses_server_toml() {
        let cfg = ServerConfig::from_toml(
            r#"
[server]
address = "0.0.0.0"
port = 7000
lobby_timeout_secs = 12

[auth]
check_player_id = true
id_files = ["auth/players.txt", "auth/extra.txt"]

[match]
players = 32
lobby = true
seed = 0
"#,
        )
        .unwrap();
        assert_eq!(cfg.server.address, "0.0.0.0");
        assert_eq!(cfg.server.port, 7000);
        assert_eq!(cfg.auth.id_files.len(), 2);
        assert_eq!(cfg.match_section.players, 32);
    }

    #[test]
    fn ruleset_tick_and_time_units() {
        let r = RulesetConfig::default().to_ruleset().unwrap();
        assert_eq!(r.lock_ticks, 30 * t99_core::FRAME_TICKS);
        assert_eq!(r.phases[0].are, 6 * t99_core::FRAME_TICKS);
        assert_eq!(r.phases[0].clear_delay, 18 * t99_core::FRAME_TICKS);
        // gravity 是 time rate，不是 tick 数
        assert_eq!(r.phases[0].gravity_base, 750);
        assert_eq!(r.match_ticks, 900 * 1000 * t99_core::TMS);
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
