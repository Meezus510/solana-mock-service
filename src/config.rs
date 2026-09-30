//! Environment-specific configuration: `config/application-${APP_ENV}.toml`.
//! `APP_ENV` is required; there is no default environment.

use std::{fmt, path::Path};

use serde::Deserialize;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AppEnv {
    Local,
    Production,
}

impl AppEnv {
    pub fn parse(value: Option<&str>) -> Result<Self, ConfigError> {
        match value.map(str::trim) {
            Some("local") => Ok(Self::Local),
            Some("production") => Ok(Self::Production),
            Some(other) => Err(ConfigError::UnknownEnvironment(other.to_owned())),
            None => Err(ConfigError::MissingEnvironment),
        }
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Production => "production",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub server: ServerConfig,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ServerConfig {
    pub host: String,
    pub port: u16,
}

impl Config {
    /// Load `application-${env}.toml` from `config_dir`.
    pub fn load(env: AppEnv, config_dir: &Path) -> Result<Self, ConfigError> {
        let path = config_dir.join(format!("application-{}.toml", env.name()));
        let text = std::fs::read_to_string(&path)
            .map_err(|error| ConfigError::Read(path.display().to_string(), error.to_string()))?;
        toml::from_str(&text)
            .map_err(|error| ConfigError::Parse(path.display().to_string(), error.to_string()))
    }
}

#[derive(Debug, Eq, PartialEq)]
pub enum ConfigError {
    MissingEnvironment,
    UnknownEnvironment(String),
    Read(String, String),
    Parse(String, String),
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingEnvironment => write!(f, "APP_ENV must be set to local or production"),
            Self::UnknownEnvironment(value) => {
                write!(f, "APP_ENV must be local or production, got {value:?}")
            }
            Self::Read(path, error) => write!(f, "cannot read {path}: {error}"),
            Self::Parse(path, error) => write!(f, "invalid config {path}: {error}"),
        }
    }
}

impl std::error::Error for ConfigError {}
