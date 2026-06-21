use anyhow::{anyhow, Result};
use std::fs;
use std::path::PathBuf;

use crate::types::Config;

pub struct ConfigManager {
    config_path: PathBuf,
}

impl ConfigManager {
    #[allow(dead_code)]
    pub fn new() -> Result<Self> {
        Self::with_path(None)
    }

    pub fn with_path(custom_path: Option<String>) -> Result<Self> {
        let config_path = if let Some(path) = custom_path {
            PathBuf::from(path)
        } else {
            let config_dir = dirs::home_dir()
                .ok_or_else(|| anyhow!("Could not find home directory"))?
                .join(".solana-validator-switch");

            // Create config directory if it doesn't exist
            if !config_dir.exists() {
                fs::create_dir_all(&config_dir)?;
            }

            config_dir.join("config.yaml")
        };

        Ok(ConfigManager { config_path })
    }

    pub fn get_config_path(&self) -> &PathBuf {
        &self.config_path
    }

    pub fn load(&self) -> Result<Config> {
        if !self.config_path.exists() {
            return Err(anyhow!(
                "Configuration file not found. Run 'svs setup' first."
            ));
        }

        let content = fs::read_to_string(&self.config_path)?;
        let content = expand_env_vars(&content)?;
        let config: Config = serde_yaml::from_str(&content)?;
        Ok(config)
    }

    #[allow(dead_code)]
    pub fn save(&self, config: &Config) -> Result<()> {
        let content = serde_yaml::to_string(config)?;
        fs::write(&self.config_path, content)?;
        Ok(())
    }

    pub fn exists(&self) -> bool {
        self.config_path.exists()
    }

    #[allow(dead_code)]
    pub fn create_default() -> Config {
        use crate::types::*;

        Config {
            version: "1.0.0".to_string(),
            validators: Vec::new(),
            verbose_logging: false,
            alert_config: None,
        }
    }
}

/// Expand `${VAR}` and `${VAR:-default}` references in the raw config text from
/// the process environment. This lets the committed config carry placeholders
/// (e.g. an RPC URL with a Helius API key) so secrets stay out of the repo.
///
/// A bare `${VAR}` with no value set and no default is an error, so a missing
/// secret fails loudly at load time instead of silently producing an empty URL.
fn expand_env_vars(content: &str) -> Result<String> {
    let mut out = String::with_capacity(content.len());
    let mut rest = content;

    while let Some(start) = rest.find("${") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let end = after
            .find('}')
            .ok_or_else(|| anyhow!("Unterminated '${{' in config (missing '}}')"))?;
        let expr = &after[..end];

        let (name, default) = match expr.split_once(":-") {
            Some((name, default)) => (name, Some(default)),
            None => (expr, None),
        };

        let value = match std::env::var(name) {
            Ok(v) => v,
            Err(_) => default
                .map(|d| d.to_string())
                .ok_or_else(|| anyhow!("Environment variable '{}' is not set", name))?,
        };
        out.push_str(&value);

        rest = &after[end + 1..];
    }
    out.push_str(rest);

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::expand_env_vars;

    #[test]
    fn expands_var_with_default_when_unset() {
        std::env::remove_var("SVS_TEST_UNSET");
        let out = expand_env_vars("rpc: ${SVS_TEST_UNSET:-https://public.example}").unwrap();
        assert_eq!(out, "rpc: https://public.example");
    }

    #[test]
    fn env_value_overrides_default() {
        std::env::set_var("SVS_TEST_SET", "https://helius.example/?api-key=secret");
        let out = expand_env_vars("rpc: ${SVS_TEST_SET:-https://public.example}").unwrap();
        assert_eq!(out, "rpc: https://helius.example/?api-key=secret");
        std::env::remove_var("SVS_TEST_SET");
    }

    #[test]
    fn missing_var_without_default_errors() {
        std::env::remove_var("SVS_TEST_REQUIRED");
        assert!(expand_env_vars("rpc: ${SVS_TEST_REQUIRED}").is_err());
    }

    #[test]
    fn leaves_plain_text_untouched() {
        let input = "rpc: https://api.mainnet-beta.solana.com\nname: foo";
        assert_eq!(expand_env_vars(input).unwrap(), input);
    }
}
