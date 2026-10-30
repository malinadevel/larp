use std::{
    fs,
    io::ErrorKind,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};

use crate::{agents::AgentCatalog, config::PromptMode, messages::Message};

pub struct Storage {
    agent_id: String,
    context_path: PathBuf,
    global_prompt_path: PathBuf,
    agent_prompt_path: PathBuf,
    config_path: PathBuf,
    security_path: PathBuf,
    tools_dir: PathBuf,
}

impl Storage {
    pub fn for_agent(catalog: &AgentCatalog, agent_id: &str) -> Result<Self> {
        let directory = catalog.agent_directory(agent_id)?;
        let root = catalog.root();
        let storage = Self {
            agent_id: agent_id.to_owned(),
            context_path: directory.join("context.json"),
            global_prompt_path: root.join("SYSTEM_PROMPT.md"),
            agent_prompt_path: directory.join("AGENT_PROMPT.md"),
            config_path: directory.join("config.toml"),
            security_path: directory.join("security.toml"),
            tools_dir: directory.join("tools"),
        };

        Ok(storage)
    }

    pub fn config_path(&self) -> &PathBuf {
        &self.config_path
    }

    pub fn security_path(&self) -> &PathBuf {
        &self.security_path
    }

    pub fn tools_dir(&self) -> &PathBuf {
        &self.tools_dir
    }

    pub fn agent_id(&self) -> &str {
        &self.agent_id
    }

    pub fn load_history(&self) -> Result<Vec<Message>> {
        let contents = match fs::read_to_string(&self.context_path) {
            Ok(contents) => contents,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("could not read {}", self.context_path.display()));
            }
        };

        serde_json::from_str(&contents)
            .with_context(|| format!("invalid JSON in {}", self.context_path.display()))
    }

    pub fn save_history(&self, messages: &[Message]) -> Result<()> {
        let contents = serde_json::to_string_pretty(messages)
            .context("could not serialize conversation history")?;
        fs::write(&self.context_path, contents)
            .with_context(|| format!("could not write {}", self.context_path.display()))
    }

    pub fn clear_history(&self) -> Result<()> {
        self.save_history(&[])
    }

    pub fn load_system_prompt(&self, mode: PromptMode) -> Result<String> {
        match mode {
            PromptMode::Global => read_prompt(&self.global_prompt_path),
            PromptMode::Agent => read_prompt(&self.agent_prompt_path),
            PromptMode::Merge => {
                let global = read_prompt(&self.global_prompt_path)?;
                let agent = read_prompt(&self.agent_prompt_path)?;
                match (global.trim().is_empty(), agent.trim().is_empty()) {
                    (false, false) => Ok(format!(
                        "## Shared instructions\n{}\n\n## Agent-specific instructions\n{}",
                        global.trim_end(),
                        agent.trim_start()
                    )),
                    (false, true) => Ok(global),
                    (true, false) => Ok(agent),
                    (true, true) => Ok(String::new()),
                }
            }
        }
    }
}

fn read_prompt(path: &Path) -> Result<String> {
    fs::read_to_string(path)
        .with_context(|| format!("could not read system prompt from {}", path.display()))
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::PathBuf,
        sync::atomic::{AtomicUsize, Ordering},
    };

    use crate::{agents::AgentCatalog, config::PromptMode};

    use super::Storage;

    static NEXT_DIRECTORY: AtomicUsize = AtomicUsize::new(0);

    fn temporary_directory() -> PathBuf {
        let sequence = NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("larp-storage-{}-{sequence}", std::process::id()));
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn prompt_modes_load_global_agent_or_merged_instructions() {
        let root = temporary_directory().join(".larp");
        let catalog = AgentCatalog::initialize_at(root.clone()).unwrap();
        let storage = Storage::for_agent(&catalog, "default").unwrap();
        fs::write(root.join("SYSTEM_PROMPT.md"), "Shared rules.\n").unwrap();
        fs::write(
            root.join("agents/default/AGENT_PROMPT.md"),
            "Agent-specific rules.\n",
        )
        .unwrap();

        assert_eq!(
            storage.load_system_prompt(PromptMode::Global).unwrap(),
            "Shared rules.\n"
        );
        assert_eq!(
            storage.load_system_prompt(PromptMode::Agent).unwrap(),
            "Agent-specific rules.\n"
        );
        assert_eq!(
            storage.load_system_prompt(PromptMode::Merge).unwrap(),
            "## Shared instructions\nShared rules.\n\n## Agent-specific instructions\nAgent-specific rules.\n"
        );

        fs::remove_dir_all(root.parent().unwrap()).unwrap();
    }

    #[test]
    fn history_is_stored_in_the_selected_agent_directory() {
        let root = temporary_directory().join(".larp");
        let mut catalog = AgentCatalog::initialize_at(root.clone()).unwrap();
        catalog.create_agent("reviewer").unwrap();
        let default = Storage::for_agent(&catalog, "default").unwrap();
        let reviewer = Storage::for_agent(&catalog, "reviewer").unwrap();

        default.save_history(&[]).unwrap();
        assert!(root.join("agents/default/context.json").is_file());
        assert!(!root.join("agents/reviewer/context.json").exists());
        assert!(reviewer.load_history().unwrap().is_empty());

        fs::remove_dir_all(root.parent().unwrap()).unwrap();
    }
}
