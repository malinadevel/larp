use std::{collections::BTreeMap, fs, path::Path};

use anyhow::{bail, Context, Result};
use serde_json::Value;

use crate::{
    messages::ToolCall,
    security::{ApprovalMode, SecurityPolicy},
};

use super::command::CommandTool;

pub struct ToolRegistry {
    tools: BTreeMap<String, CommandTool>,
    security: SecurityPolicy,
}

impl ToolRegistry {
    pub fn load(directory: &Path, security: SecurityPolicy) -> Result<Self> {
        let entries = match fs::read_dir(directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self {
                    tools: BTreeMap::new(),
                    security,
                });
            }
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("could not read tools directory {}", directory.display())
                });
            }
        };

        let mut manifests = entries
            .map(|entry| {
                entry
                    .map(|entry| entry.path())
                    .with_context(|| format!("could not read an entry in {}", directory.display()))
            })
            .collect::<Result<Vec<_>>>()?;
        manifests.retain(|path| {
            path.extension()
                .is_some_and(|extension| extension == "toml")
        });
        manifests.sort();

        let mut tools = BTreeMap::new();
        for path in manifests {
            let tool = CommandTool::load(&path)?;
            let id = tool.id().to_owned();
            if tools.insert(id.clone(), tool).is_some() {
                bail!("duplicate tool id {id:?} (from {})", path.display());
            }
        }

        Ok(Self { tools, security })
    }

    pub fn definitions(&self) -> Vec<Value> {
        self.tools
            .values()
            .filter(|tool| self.security.is_enabled(tool.id()))
            .map(CommandTool::definition)
            .collect()
    }

    pub async fn execute(&self, call: &ToolCall) -> String {
        let Some(tool) = self.tools.get(&call.function.name) else {
            return format!("Unknown tool: {}", call.function.name);
        };
        if !self.security.is_enabled(tool.id()) {
            return format!("Tool {} is disabled by security policy.", tool.id());
        }

        let arguments: Value = match serde_json::from_str(&call.function.arguments) {
            Ok(arguments) => arguments,
            Err(error) => return format!("Invalid arguments for {}: {error}", tool.id()),
        };
        let prepared = match tool.prepare(&arguments) {
            Ok(prepared) => prepared,
            Err(error) => return format!("Invalid arguments for {}: {error:#}", tool.id()),
        };

        let environment = self.security.environment_mode(tool.id());
        if self.security.approval_mode(tool.id()) == ApprovalMode::Always {
            let summary = format!(
                "{}\nОкружение: {}",
                tool.approval_summary(&prepared),
                environment.approval_description()
            );
            match self.security.confirm(tool.id(), &summary) {
                Ok(true) => {}
                Ok(false) => return "The user denied permission to run this tool.".to_owned(),
                Err(error) => return format!("Could not request approval: {error:#}"),
            }
        }

        match tool.execute(prepared, environment).await {
            Ok(output) => output,
            Err(error) => format!("Tool {} failed: {error:#}", tool.id()),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::PathBuf,
        sync::atomic::{AtomicUsize, Ordering},
    };

    use crate::{
        messages::{FunctionCall, ToolCall},
        security::SecurityPolicy,
    };

    use super::ToolRegistry;

    static NEXT_DIRECTORY: AtomicUsize = AtomicUsize::new(0);

    fn temporary_directory() -> PathBuf {
        let sequence = NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "larp-tool-registry-{}-{sequence}",
            std::process::id()
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn registry_has_no_tools_hardcoded_in_rust() {
        let directory = temporary_directory();
        let registry = ToolRegistry::load(&directory, test_policy(true)).unwrap();

        assert!(registry.definitions().is_empty());
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn starter_bash_and_read_file_tools_load_from_templates() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let tools_dir = root.join("templates/agents/default/tools");
        let security =
            SecurityPolicy::load(&root.join("templates/agents/default/security.toml")).unwrap();
        let registry = ToolRegistry::load(&tools_dir, security).unwrap();

        let names = registry
            .definitions()
            .iter()
            .map(|definition| definition["function"]["name"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>();
        assert_eq!(names, ["bash", "read_file"]);
    }

    #[test]
    fn registry_loads_tool_definitions_from_toml() {
        let directory = temporary_directory();
        fs::write(directory.join("read_file.toml"), read_file_manifest()).unwrap();

        let registry = ToolRegistry::load(&directory, test_policy(true)).unwrap();
        let definitions = registry.definitions();

        assert_eq!(definitions.len(), 1);
        assert_eq!(definitions[0]["function"]["name"], "read_file");
        assert_eq!(
            definitions[0]["function"]["parameters"]["properties"]["path"]["type"],
            "string"
        );
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn registry_rejects_duplicate_tool_ids() {
        let directory = temporary_directory();
        fs::write(directory.join("first.toml"), command_manifest("read_file")).unwrap();
        fs::write(directory.join("second.toml"), command_manifest("read_file")).unwrap();

        assert!(ToolRegistry::load(&directory, test_policy(true)).is_err());
        fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn disabled_tools_are_not_advertised_or_executable() {
        let directory = temporary_directory();
        fs::write(directory.join("read_file.toml"), read_file_manifest()).unwrap();

        let registry = ToolRegistry::load(&directory, test_policy(false)).unwrap();
        assert!(registry.definitions().is_empty());
        let result = registry
            .execute(&tool_call(
                "read_file",
                serde_json::json!({"path":"/etc/hostname"}).to_string(),
            ))
            .await;
        assert!(result.contains("disabled by security policy"));
        fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn command_tool_runs_directly_and_returns_captured_output() {
        let directory = temporary_directory();
        fs::write(directory.join("echo_stdin.toml"), echo_stdin_manifest()).unwrap();
        let registry = ToolRegistry::load(&directory, test_policy(true)).unwrap();

        let result = registry
            .execute(&tool_call(
                "echo_stdin",
                r#"{"content":"hello from process"}"#.to_owned(),
            ))
            .await;

        assert!(result.contains("exit_code: 0"));
        assert!(result.contains("hello from process"));
        fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn bash_is_loaded_and_executed_through_the_command_manifest() {
        let directory = temporary_directory();
        fs::write(directory.join("bash.toml"), bash_manifest()).unwrap();
        let registry = ToolRegistry::load(&directory, test_policy(true)).unwrap();

        let result = registry
            .execute(&tool_call(
                "bash",
                r#"{"command":"printf 'bash through command executor'"}"#.to_owned(),
            ))
            .await;

        assert!(result.contains("exit_code: 0"));
        assert!(result.contains("bash through command executor"));
        fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn command_tool_maps_a_path_to_a_single_argv_value() {
        let directory = temporary_directory();
        let file_path = directory.join("file name with spaces.txt");
        fs::write(&file_path, "contents from cat").unwrap();
        fs::write(directory.join("read_file.toml"), read_file_manifest()).unwrap();
        let registry = ToolRegistry::load(&directory, test_policy(true)).unwrap();

        let result = registry
            .execute(&tool_call(
                "read_file",
                serde_json::json!({"path": file_path}).to_string(),
            ))
            .await;

        assert!(result.contains("exit_code: 0"));
        assert!(result.contains("contents from cat"));
        fs::remove_dir_all(directory).unwrap();
    }

    fn test_policy(read_file_enabled: bool) -> SecurityPolicy {
        static NEXT_POLICY: AtomicUsize = AtomicUsize::new(0);
        let sequence = NEXT_POLICY.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "larp-test-security-{}-{sequence}.toml",
            std::process::id(),
        ));
        fs::write(
            &path,
            format!(
                "[defaults]\nenabled = false\napproval = 'always'\nenvironment = 'clean'\n\
                 [tools.bash]\nenabled = true\napproval = 'never'\nenvironment = 'inherit'\n\
                 [tools.read_file]\nenabled = {read_file_enabled}\napproval = 'never'\nenvironment = 'clean'\n\
                 [tools.echo_stdin]\nenabled = true\napproval = 'never'\nenvironment = 'clean'\n"
            ),
        )
        .unwrap();
        let policy = SecurityPolicy::load(&path).unwrap();
        fs::remove_file(path).unwrap();
        policy
    }

    fn tool_call(name: &str, arguments: String) -> ToolCall {
        ToolCall {
            id: "call_test".to_owned(),
            call_type: "function".to_owned(),
            function: FunctionCall {
                name: name.to_owned(),
                arguments,
            },
        }
    }

    fn read_file_manifest() -> &'static str {
        r#"
id = "read_file"
version = "1"
description = "Read a text file"
program = "/usr/bin/cat"
timeout_ms = 30000
max_output_bytes = 16000
argv = [{ literal = "--" }, { from = "path" }]

[input_schema]
type = "object"
required = ["path"]
additionalProperties = false

[input_schema.properties.path]
type = "string"
"#
    }

    fn echo_stdin_manifest() -> &'static str {
        r#"
id = "echo_stdin"
version = "1"
description = "Echo stdin for a tool executor test"
program = "/usr/bin/cat"
timeout_ms = 30000
max_output_bytes = 16000
stdin_from = "content"

[input_schema]
type = "object"
required = ["content"]
additionalProperties = false

[input_schema.properties.content]
type = "string"
"#
    }

    fn bash_manifest() -> &'static str {
        r#"
id = "bash"
version = "1"
description = "Run a command in bash"
program = "/usr/bin/bash"
timeout_ms = 30000
max_output_bytes = 16000
argv = [{ literal = "-lc" }, { from = "command" }]

[input_schema]
type = "object"
required = ["command"]
additionalProperties = false

[input_schema.properties.command]
type = "string"
"#
    }

    fn command_manifest(id: &str) -> String {
        format!(
            r#"
id = "{id}"
version = "1"
description = "Test command"
program = "/usr/bin/cat"
timeout_ms = 30000
max_output_bytes = 16000

[input_schema]
type = "object"
additionalProperties = false
"#
        )
    }
}
