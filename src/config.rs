use std::{env, fs, path::Path};

use anyhow::{bail, Context, Result};
use serde::Deserialize;

#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum PromptMode {
    Global,
    Agent,
    #[default]
    Merge,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct PromptConfig {
    #[serde(default)]
    mode: PromptMode,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileConfig {
    model: String,
    endpoint: String,
    api_key_env: String,
    max_tool_rounds: usize,
    #[serde(default)]
    prompt: PromptConfig,
}

pub struct Config {
    pub api_key: String,
    pub model: String,
    pub endpoint: String,
    pub max_tool_rounds: usize,
    pub prompt_mode: PromptMode,
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let contents = fs::read_to_string(path)
            .with_context(|| format!("could not read config from {}", path.display()))?;

        let file_config: FileConfig = toml::from_str(&contents)
            .with_context(|| format!("invalid TOML in {}", path.display()))?;
        Self::from_file_config(file_config)
    }

    fn from_file_config(file_config: FileConfig) -> Result<Self> {
        if file_config.model.trim().is_empty() {
            bail!("model in config must not be empty");
        }
        if !(file_config.endpoint.starts_with("https://")
            || file_config.endpoint.starts_with("http://"))
        {
            bail!("endpoint in config must be an HTTP or HTTPS URL");
        }
        if !(1..=100).contains(&file_config.max_tool_rounds) {
            bail!("max_tool_rounds in config must be between 1 and 100");
        }
        if file_config.api_key_env.trim().is_empty() {
            bail!("api_key_env in config must not be empty");
        }

        let api_key = env::var(&file_config.api_key_env).map_err(|_| {
            anyhow::anyhow!(
                "environment variable {} is not set",
                file_config.api_key_env
            )
        })?;

        if api_key.trim().is_empty() {
            bail!("environment variable {} is empty", file_config.api_key_env);
        }

        Ok(Self {
            api_key,
            model: file_config.model,
            endpoint: file_config.endpoint,
            max_tool_rounds: file_config.max_tool_rounds,
            prompt_mode: file_config.prompt.mode,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{FileConfig, PromptMode};

    #[test]
    fn generated_config_contains_all_required_fields() {
        let config: FileConfig =
            toml::from_str(include_str!("../templates/agents/default/config.toml")).unwrap();

        assert_eq!(config.model, "z-ai/glm-5.3-flash");
        assert_eq!(
            config.endpoint,
            "https://openrouter.ai/api/v1/chat/completions"
        );
        assert_eq!(config.api_key_env, "TEST_API_KEY");
        assert_eq!(config.max_tool_rounds, 8);
        assert_eq!(config.prompt.mode, PromptMode::Merge);
    }

    #[test]
    fn required_config_fields_cannot_be_omitted() {
        let result = toml::from_str::<FileConfig>("model = 'some-model'");

        assert!(result.is_err());
    }

    #[test]
    fn unknown_config_fields_are_rejected() {
        let result = toml::from_str::<FileConfig>("modle = 'typo'");

        assert!(result.is_err());
    }

    #[test]
    fn prompt_mode_accepts_only_supported_values_and_defaults_for_legacy_configs() {
        for (value, expected) in [
            ("global", PromptMode::Global),
            ("agent", PromptMode::Agent),
            ("merge", PromptMode::Merge),
        ] {
            let config: FileConfig = toml::from_str(&format!(
                "model='m'\nendpoint='https://example.test'\napi_key_env='KEY'\n\
                 max_tool_rounds=1\n[prompt]\nmode='{value}'"
            ))
            .unwrap();
            assert_eq!(config.prompt.mode, expected);
        }

        let legacy: FileConfig = toml::from_str(
            "model='m'\nendpoint='https://example.test'\napi_key_env='KEY'\nmax_tool_rounds=1",
        )
        .unwrap();
        assert_eq!(legacy.prompt.mode, PromptMode::Merge);

        let invalid: Result<FileConfig, _> = toml::from_str(
            "model='m'\nendpoint='https://example.test'\napi_key_env='KEY'\n\
             max_tool_rounds=1\n[prompt]\nmode='both'",
        );
        assert!(invalid.is_err());
    }
}
