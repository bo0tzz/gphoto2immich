use std::env;

use anyhow::{anyhow, Context, Result};

#[derive(Debug, Clone)]
pub struct Config {
    pub immich_url: String,
    pub immich_api_key: String,
    pub stack_jpeg_raf: bool,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        Ok(Config {
            immich_url: require_var("IMMICH_URL")?,
            immich_api_key: require_var("IMMICH_API_KEY")?,
            stack_jpeg_raf: parse_bool_env("STACK_JPEG_RAF", true)?,
        })
    }
}

fn require_var(name: &str) -> Result<String> {
    env::var(name).map_err(|_| anyhow!("required env var {name} is not set"))
}

fn parse_bool_env(name: &str, default: bool) -> Result<bool> {
    match env::var(name) {
        Ok(v) => match v.to_ascii_lowercase().as_str() {
            "true" | "1" | "yes" | "on" => Ok(true),
            "false" | "0" | "no" | "off" => Ok(false),
            other => Err(anyhow!(
                "{name} must be true/false (got {other:?})"
            )),
        },
        Err(_) => Ok(default),
    }
    .with_context(|| format!("parsing env var {name}"))
}
