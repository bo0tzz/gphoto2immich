use std::env;

use anyhow::{anyhow, Context, Result};
use chrono_tz::Tz;

#[derive(Debug, Clone)]
pub struct Config {
    pub immich_url: String,
    pub immich_api_key: String,
    /// IANA timezone the camera's clock is set to. libgphoto2 reports
    /// `mtime` as camera-local wall-clock seconds reinterpreted as Unix
    /// epoch, so we need to know what TZ to map it from.
    pub camera_tz: Tz,
    pub stack_jpeg_raf: bool,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        Ok(Config {
            immich_url: require_var("IMMICH_URL")?,
            immich_api_key: require_var("IMMICH_API_KEY")?,
            camera_tz: parse_tz(&require_var("FUJI_TZ")?)?,
            stack_jpeg_raf: parse_bool_env("STACK_JPEG_RAF", true)?,
        })
    }
}

fn require_var(name: &str) -> Result<String> {
    env::var(name).map_err(|_| anyhow!("required env var {name} is not set"))
}

fn parse_tz(s: &str) -> Result<Tz> {
    s.parse::<Tz>()
        .map_err(|e| anyhow!("invalid FUJI_TZ {s:?}: {e}"))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_iana_tz() {
        assert_eq!(
            parse_tz("America/Los_Angeles").unwrap().name(),
            "America/Los_Angeles"
        );
    }

    #[test]
    fn rejects_garbage_tz() {
        assert!(parse_tz("Mars/Olympus_Mons").is_err());
    }
}
