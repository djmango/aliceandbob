use std::collections::HashMap;
use std::path::Path;

use anyhow::{bail, Context, Result};
use serde::Deserialize;

use crate::pb::aliceandbob::v1 as pb;

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub server: ServerConfig,
    pub providers: HashMap<String, ProviderConfig>,
    #[serde(default)]
    pub agents: Vec<AgentEntry>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ServerConfig {
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default = "default_db")]
    pub database: String,
    /// Directory with the built web UI (vite `dist`). Served at / when set
    /// and present, so a single binary/container hosts API + UI.
    #[serde(default)]
    pub web_dist: Option<String>,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            port: default_port(),
            database: default_db(),
            web_dist: None,
        }
    }
}

fn default_port() -> u16 {
    3030
}

fn default_db() -> String {
    "aliceandbob.sqlite".to_string()
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProviderConfig {
    /// OpenAI-compatible base url, e.g. "https://openrouter.ai/api/v1".
    pub base_url: String,
    /// Literal API key (prefer api_key_env).
    pub api_key: Option<String>,
    /// Name of an environment variable holding the API key.
    pub api_key_env: Option<String>,
    /// Max in-flight requests to this provider (queue size gate).
    #[serde(default = "default_max_concurrent")]
    pub max_concurrent: u32,
    /// Pace request starts to this rate. Essential for free tiers
    /// (e.g. Groq free = 30 RPM; set ~25 to leave headroom).
    pub requests_per_minute: Option<u32>,
}

fn default_max_concurrent() -> u32 {
    4
}

impl ProviderConfig {
    pub fn resolve_api_key(&self) -> Option<String> {
        if let Some(k) = &self.api_key {
            return Some(k.clone());
        }
        if let Some(env) = &self.api_key_env {
            return std::env::var(env).ok();
        }
        None
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct AgentEntry {
    pub id: String,
    pub name: String,
    /// "player" or "game_master"
    pub role: String,
    pub provider: String,
    pub model: String,
    #[serde(default)]
    pub persona: String,
}

impl AgentEntry {
    pub fn to_proto(&self) -> pb::AgentConfig {
        pb::AgentConfig {
            id: self.id.clone(),
            name: self.name.clone(),
            role: match self.role.as_str() {
                "game_master" | "gm" => pb::AgentRole::GameMaster as i32,
                "player" => pb::AgentRole::Player as i32,
                _ => pb::AgentRole::Unspecified as i32,
            },
            provider: self.provider.clone(),
            model: self.model.clone(),
            persona: self.persona.clone(),
        }
    }

    pub fn is_player(&self) -> bool {
        self.role == "player"
    }

    pub fn is_gm(&self) -> bool {
        matches!(self.role.as_str(), "game_master" | "gm")
    }
}

impl Config {
    /// Loads providers.toml from ./ or ../ (so `cargo run` works from server/).
    pub fn load() -> Result<Self> {
        let candidates = ["providers.toml", "../providers.toml"];
        for path in candidates {
            if Path::new(path).exists() {
                let raw = std::fs::read_to_string(path)
                    .with_context(|| format!("reading {path}"))?;
                let config: Config =
                    toml::from_str(&raw).with_context(|| format!("parsing {path}"))?;
                config.validate()?;
                tracing::info!(path, "loaded config");
                return Ok(config);
            }
        }
        bail!("providers.toml not found (looked in . and ..). Copy providers.example.toml to providers.toml")
    }

    fn validate(&self) -> Result<()> {
        for agent in &self.agents {
            if !self.providers.contains_key(&agent.provider) {
                bail!(
                    "agent '{}' references unknown provider '{}'",
                    agent.id,
                    agent.provider
                );
            }
        }
        Ok(())
    }

    pub fn agent(&self, id: &str) -> Option<&AgentEntry> {
        self.agents.iter().find(|a| a.id == id)
    }

    pub fn default_gm(&self) -> Option<&AgentEntry> {
        self.agents.iter().find(|a| a.is_gm())
    }

    pub fn players(&self) -> impl Iterator<Item = &AgentEntry> {
        self.agents.iter().filter(|a| a.is_player())
    }
}
