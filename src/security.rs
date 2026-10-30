use std::{
    collections::BTreeMap,
    fs,
    io::{self, Write},
    path::Path,
};

use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ApprovalMode {
    Always,
    Never,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum EnvironmentMode {
    Clean,
    Inherit,
}

impl EnvironmentMode {
    pub fn approval_description(self) -> &'static str {
        match self {
            Self::Clean => "очищенное окружение с ограниченным PATH",
            Self::Inherit => "окружение процесса, включая доступные ему переменные",
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
struct ToolRule {
    enabled: bool,
    approval: ApprovalMode,
    #[serde(default)]
    environment: Option<EnvironmentMode>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SecurityFile {
    defaults: ToolRule,
    #[serde(default)]
    tools: BTreeMap<String, ToolRule>,
}

pub struct SecurityPolicy {
    defaults: ToolRule,
    tools: BTreeMap<String, ToolRule>,
}

impl SecurityPolicy {
    pub fn load(path: &Path) -> Result<Self> {
        let contents = fs::read_to_string(path)
            .with_context(|| format!("could not read security policy {}", path.display()))?;
        let file: SecurityFile = toml::from_str(&contents)
            .with_context(|| format!("invalid TOML in security policy {}", path.display()))?;

        Ok(Self {
            defaults: file.defaults,
            tools: file.tools,
        })
    }

    pub fn is_enabled(&self, tool_id: &str) -> bool {
        self.rule(tool_id).enabled
    }

    pub fn approval_mode(&self, tool_id: &str) -> ApprovalMode {
        self.rule(tool_id).approval
    }

    pub fn environment_mode(&self, tool_id: &str) -> EnvironmentMode {
        self.rule(tool_id).environment.unwrap_or_else(|| {
            // Preserve the pre-policy behavior for an existing security.toml.
            if tool_id == "bash" {
                EnvironmentMode::Inherit
            } else {
                EnvironmentMode::Clean
            }
        })
    }

    pub fn confirm(&self, tool_id: &str, summary: &str) -> Result<bool> {
        println!("\nВызов инструмента: {tool_id}");
        for line in summary.lines() {
            println!("  {line}");
        }
        print!("Выполнить? [y/N] ");
        io::stdout()
            .flush()
            .context("could not flush terminal output")?;

        let mut answer = String::new();
        io::stdin()
            .read_line(&mut answer)
            .context("could not read confirmation")?;

        Ok(matches!(
            answer.trim().to_ascii_lowercase().as_str(),
            "y" | "yes"
        ))
    }

    fn rule(&self, tool_id: &str) -> ToolRule {
        self.tools.get(tool_id).copied().unwrap_or(self.defaults)
    }
}

#[cfg(test)]
mod tests {
    use super::{EnvironmentMode, SecurityFile};

    #[test]
    fn default_policy_enables_tools_and_approves_them() {
        let policy: SecurityFile =
            toml::from_str(include_str!("../templates/agents/default/security.toml")).unwrap();

        assert!(policy.defaults.enabled);
        assert_eq!(policy.defaults.approval, super::ApprovalMode::Always);
        let bash = policy.tools.get("bash").unwrap();
        assert!(bash.enabled);
        assert_eq!(bash.approval, super::ApprovalMode::Always);
        assert_eq!(bash.environment, Some(EnvironmentMode::Inherit));

        let read_file = policy.tools.get("read_file").unwrap();
        assert!(read_file.enabled);
        assert_eq!(read_file.approval, super::ApprovalMode::Always);
        assert_eq!(read_file.environment, Some(EnvironmentMode::Clean));
    }

    #[test]
    fn policy_rejects_unknown_fields_and_approval_modes() {
        assert!(toml::from_str::<SecurityFile>(
            "[defaults]\nenabled = false\napproval = 'sometimes'"
        )
        .is_err());

        assert!(toml::from_str::<SecurityFile>(
            "[defaults]\nenabled = false\napproval = 'always'\nunknown = true"
        )
        .is_err());
    }

    #[test]
    fn old_security_files_keep_bash_environment_behavior() {
        let file: SecurityFile = toml::from_str(
            "[defaults]\nenabled = false\napproval = 'always'\n\
             [tools.bash]\nenabled = true\napproval = 'always'",
        )
        .unwrap();
        let policy = super::SecurityPolicy {
            defaults: file.defaults,
            tools: file.tools,
        };

        assert_eq!(policy.environment_mode("bash"), EnvironmentMode::Inherit);
        assert_eq!(
            policy.environment_mode("custom_tool"),
            EnvironmentMode::Clean
        );
    }
}
