use std::{
    collections::HashSet,
    env, fs,
    path::{Path, PathBuf},
};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

const MAX_AGENT_ID_LENGTH: usize = 64;

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AgentEntry {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RegistryFile {
    default_agent: String,
    agents: Vec<AgentEntry>,
}

pub struct AgentCatalog {
    root: PathBuf,
    registry_path: PathBuf,
    registry: RegistryFile,
}

impl AgentCatalog {
    pub fn initialize() -> Result<Self> {
        let home = env::var_os("HOME")
            .or_else(|| env::var_os("USERPROFILE"))
            .map(PathBuf::from)
            .context("could not determine the home directory (HOME or USERPROFILE is unset)")?;
        Self::initialize_at(home.join(".larp"))
    }

    pub(crate) fn initialize_at(root: PathBuf) -> Result<Self> {
        fs::create_dir_all(&root)
            .with_context(|| format!("could not create {}", root.display()))?;
        migrate_legacy_layout(&root)?;
        crate::templates::install_defaults(&root)?;

        let registry_path = root.join("agents/index.toml");
        let contents = fs::read_to_string(&registry_path).with_context(|| {
            format!("could not read agent registry {}", registry_path.display())
        })?;
        let registry: RegistryFile = toml::from_str(&contents)
            .with_context(|| format!("invalid agent registry {}", registry_path.display()))?;
        validate_registry(&registry, &root)?;

        Ok(Self {
            root,
            registry_path,
            registry,
        })
    }

    pub fn root(&self) -> &PathBuf {
        &self.root
    }

    pub fn default_agent_id(&self) -> &str {
        &self.registry.default_agent
    }

    pub fn agents(&self) -> &[AgentEntry] {
        &self.registry.agents
    }

    pub fn agent_directory(&self, id: &str) -> Result<PathBuf> {
        validate_agent_id(id)?;
        if !self.registry.agents.iter().any(|agent| agent.id == id) {
            bail!("unknown agent {id:?}; run `larp agents list` to see available agents");
        }
        Ok(self.root.join("agents").join(id))
    }

    pub fn create_agent(&mut self, id: &str) -> Result<()> {
        validate_agent_id(id)?;
        if self.registry.agents.iter().any(|agent| agent.id == id) {
            bail!("agent {id:?} already exists");
        }

        let directory = self.root.join("agents").join(id);
        if directory.exists() {
            bail!(
                "agent directory already exists but is not registered: {}",
                directory.display()
            );
        }

        let template = crate::templates::template_directory()?.join("agents/default");
        crate::templates::copy_missing_tree(&template, &directory)
            .with_context(|| format!("could not create agent directory {}", directory.display()))?;

        let mut registry = self.registry.clone();
        registry.agents.push(AgentEntry {
            id: id.to_owned(),
            name: id.to_owned(),
        });
        self.write_registry(&registry)?;
        self.registry = registry;
        Ok(())
    }

    pub fn set_default_agent(&mut self, id: &str) -> Result<()> {
        if !self.registry.agents.iter().any(|agent| agent.id == id) {
            bail!("unknown agent {id:?}; run `larp agents list` to see available agents");
        }
        let mut registry = self.registry.clone();
        registry.default_agent = id.to_owned();
        self.write_registry(&registry)?;
        self.registry = registry;
        Ok(())
    }

    fn write_registry(&self, registry: &RegistryFile) -> Result<()> {
        let serialized = format!(
            "# Agent registry. This file lists agents available to `larp --agent <id>`.\n\
             # Agent directories are named after their IDs and must be direct children of agents/.\n\
             # IDs must contain 1-64 ASCII letters, digits, '_' or '-'. IDs must be unique.\n\
             # `default_agent` must refer to one of the listed IDs.\n\n\
             # Agent selected when --agent is not provided.\n\n{}",
            toml::to_string_pretty(registry).context("could not serialize agent registry")?
        );
        let temporary_path = self.registry_path.with_extension("toml.tmp");
        fs::write(&temporary_path, serialized)
            .with_context(|| format!("could not write {}", temporary_path.display()))?;
        if let Err(error) = fs::rename(&temporary_path, &self.registry_path) {
            let _ = fs::remove_file(&temporary_path);
            return Err(error).with_context(|| {
                format!(
                    "could not replace agent registry {}",
                    self.registry_path.display()
                )
            });
        }
        Ok(())
    }
}

fn validate_registry(registry: &RegistryFile, root: &Path) -> Result<()> {
    let mut ids = HashSet::new();
    for agent in &registry.agents {
        validate_agent_id(&agent.id).with_context(|| format!("invalid agent ID {:?}", agent.id))?;
        if agent.name.trim().is_empty() {
            bail!("agent {:?} has an empty name", agent.id);
        }
        if !ids.insert(&agent.id) {
            bail!("duplicate agent ID {:?} in registry", agent.id);
        }
        let directory = root.join("agents").join(&agent.id);
        if !directory.is_dir() {
            bail!("agent directory does not exist: {}", directory.display());
        }
    }
    if !ids.contains(&registry.default_agent) {
        bail!(
            "default_agent {:?} is not listed in the agent registry",
            registry.default_agent
        );
    }
    Ok(())
}

fn validate_agent_id(id: &str) -> Result<()> {
    if id.is_empty()
        || id.len() > MAX_AGENT_ID_LENGTH
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        bail!("agent ID must contain 1-64 ASCII letters, digits, underscores, or hyphens");
    }
    Ok(())
}

fn migrate_legacy_layout(root: &Path) -> Result<()> {
    let registry_path = root.join("agents/index.toml");
    if registry_path.exists() {
        return Ok(());
    }

    let legacy_tools = root.join("tools");
    let legacy_files = ["config.toml", "security.toml", "context.json"];
    let has_legacy_layout =
        legacy_files.iter().any(|name| root.join(name).is_file()) || legacy_tools.is_dir();
    if !has_legacy_layout {
        return Ok(());
    }

    let default_directory = root.join("agents/default");
    fs::create_dir_all(&default_directory)
        .with_context(|| format!("could not create {}", default_directory.display()))?;

    for name in legacy_files {
        let source = root.join(name);
        let destination = default_directory.join(name);
        if source.is_file() && !destination.exists() {
            fs::copy(&source, &destination).with_context(|| {
                format!(
                    "could not migrate {} to {}",
                    source.display(),
                    destination.display()
                )
            })?;
        }
    }

    if legacy_tools.is_dir() {
        crate::templates::copy_missing_tree(&legacy_tools, &default_directory.join("tools"))
            .context("could not migrate legacy tools into the default agent")?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::PathBuf,
        sync::atomic::{AtomicUsize, Ordering},
    };

    use super::AgentCatalog;

    static NEXT_DIRECTORY: AtomicUsize = AtomicUsize::new(0);

    fn temporary_directory() -> PathBuf {
        let sequence = NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("larp-agents-{}-{sequence}", std::process::id()));
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn initializes_default_agent_and_installs_agent_templates() {
        let root = temporary_directory().join(".larp");
        let catalog = AgentCatalog::initialize_at(root.clone()).unwrap();

        assert_eq!(catalog.default_agent_id(), "default");
        assert!(root.join("SYSTEM_PROMPT.md").is_file());
        assert!(root.join("agents/default/AGENT_PROMPT.md").is_file());
        assert!(root.join("agents/default/config.toml").is_file());
        assert!(root.join("agents/default/security.toml").is_file());
        assert!(root.join("agents/default/tools/bash.toml").is_file());

        fs::remove_dir_all(root.parent().unwrap()).unwrap();
    }

    #[test]
    fn migrates_flat_configuration_and_history_without_removing_sources() {
        let root = temporary_directory().join(".larp");
        fs::create_dir_all(root.join("tools")).unwrap();
        fs::write(root.join("SYSTEM_PROMPT.md"), "legacy common prompt\n").unwrap();
        fs::write(root.join("config.toml"), "model = 'custom-model'\n").unwrap();
        fs::write(root.join("security.toml"), "legacy security\n").unwrap();
        fs::write(root.join("context.json"), "[]\n").unwrap();
        fs::write(root.join("tools/custom.toml"), "legacy tool\n").unwrap();
        let script = root.join("tools/custom.sh");
        fs::write(&script, "#!/bin/sh\nexit 0\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        }

        AgentCatalog::initialize_at(root.clone()).unwrap();

        assert_eq!(
            fs::read_to_string(root.join("agents/default/config.toml")).unwrap(),
            "model = 'custom-model'\n"
        );
        assert_eq!(
            fs::read_to_string(root.join("agents/default/context.json")).unwrap(),
            "[]\n"
        );
        assert_eq!(
            fs::read_to_string(root.join("agents/default/tools/custom.toml")).unwrap(),
            "legacy tool\n"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let permissions = fs::metadata(root.join("agents/default/tools/custom.sh"))
                .unwrap()
                .permissions();
            assert_ne!(permissions.mode() & 0o111, 0);
        }
        assert_eq!(
            fs::read_to_string(root.join("SYSTEM_PROMPT.md")).unwrap(),
            "legacy common prompt\n"
        );
        assert!(root.join("config.toml").is_file());

        fs::remove_dir_all(root.parent().unwrap()).unwrap();
    }

    #[test]
    fn creates_agents_and_changes_the_default() {
        let root = temporary_directory().join(".larp");
        let mut catalog = AgentCatalog::initialize_at(root.clone()).unwrap();

        catalog.create_agent("reviewer").unwrap();
        catalog.set_default_agent("reviewer").unwrap();

        assert_eq!(catalog.default_agent_id(), "reviewer");
        assert!(root.join("agents/reviewer/AGENT_PROMPT.md").is_file());
        assert!(catalog.agent_directory("reviewer").is_ok());
        assert!(catalog.create_agent("../outside").is_err());

        let reloaded = AgentCatalog::initialize_at(root.clone()).unwrap();
        assert_eq!(reloaded.default_agent_id(), "reviewer");
        assert_eq!(reloaded.agents().len(), 2);

        fs::remove_dir_all(root.parent().unwrap()).unwrap();
    }
}
