use std::{
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    process::{Child, Command},
    time::timeout,
};

use crate::security::EnvironmentMode;

const COMMAND_PATH: &str = "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";
const MAX_ALLOWED_TIMEOUT_MS: u64 = 300_000;
const MAX_ALLOWED_OUTPUT_BYTES: usize = 1_000_000;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CommandManifest {
    id: String,
    version: String,
    description: String,
    program: PathBuf,
    input_schema: InputSchema,
    #[serde(default)]
    argv: Vec<ArgumentMapping>,
    #[serde(default)]
    stdin_from: Option<String>,
    // Accepted for compatibility with early manifests; security.toml is authoritative.
    #[serde(default, rename = "approval_required")]
    _legacy_approval_required: Option<bool>,
    timeout_ms: u64,
    max_output_bytes: usize,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct InputSchema {
    #[serde(rename = "type")]
    schema_type: String,
    #[serde(default)]
    properties: std::collections::BTreeMap<String, PropertySchema>,
    #[serde(default)]
    required: Vec<String>,
    #[serde(default, rename = "additionalProperties")]
    additional_properties: bool,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct PropertySchema {
    #[serde(rename = "type")]
    schema_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    description: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ArgumentMapping {
    #[serde(default)]
    literal: Option<String>,
    #[serde(default)]
    from: Option<String>,
}

pub struct CommandTool {
    manifest: CommandManifest,
    program: PathBuf,
}

pub struct PreparedCommand {
    argv: Vec<String>,
    stdin_data: Option<Vec<u8>>,
}

impl CommandTool {
    pub fn load(path: &Path) -> Result<Self> {
        let contents = std::fs::read_to_string(path)
            .with_context(|| format!("could not read tool manifest {}", path.display()))?;
        let mut manifest: CommandManifest = toml::from_str(&contents)
            .with_context(|| format!("invalid TOML in tool manifest {}", path.display()))?;

        validate_manifest(&manifest)
            .with_context(|| format!("invalid tool manifest {}", path.display()))?;

        let program = resolve_program(path, &manifest.program)?;
        manifest.program = program.clone();

        Ok(Self { manifest, program })
    }

    pub fn id(&self) -> &str {
        &self.manifest.id
    }

    pub fn definition(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": self.manifest.id,
                "description": self.manifest.description,
                "parameters": serde_json::to_value(&self.manifest.input_schema)
                    .expect("validated schema should serialize to JSON"),
            }
        })
    }

    pub fn prepare(&self, arguments: &Value) -> Result<PreparedCommand> {
        validate_arguments(&self.manifest.input_schema, arguments)?;
        let argv = self.build_argv(arguments)?;
        let stdin_data = self.stdin_data(arguments)?;

        Ok(PreparedCommand { argv, stdin_data })
    }

    pub fn approval_summary(&self, prepared: &PreparedCommand) -> String {
        let cwd = std::env::current_dir()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|_| "<unknown>".to_owned());
        let mut summary = format!(
            "Рабочая директория: {cwd}\nПрограмма: {}\nАргументы: {:?}",
            self.program.display(),
            prepared.argv
        );

        if let Some(stdin_data) = &prepared.stdin_data {
            let preview = String::from_utf8_lossy(stdin_data);
            let mut end = preview.len().min(1_000);
            while !preview.is_char_boundary(end) {
                end -= 1;
            }
            summary.push_str(&format!("\nstdin ({} байт):\n", stdin_data.len()));
            summary.push_str(&preview[..end]);
            if end < preview.len() {
                summary.push_str("\n[preview truncated]");
            }
        }

        summary
    }

    pub async fn execute(
        &self,
        prepared: PreparedCommand,
        environment: EnvironmentMode,
    ) -> Result<String> {
        let output = self
            .run(&prepared.argv, prepared.stdin_data, environment)
            .await?;
        let exit_code = output.status.code().map_or_else(
            || "terminated by signal".to_owned(),
            |code| code.to_string(),
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout_suffix = if output.stdout_truncated {
            "\n[output truncated]"
        } else {
            ""
        };
        let stderr_suffix = if output.stderr_truncated {
            "\n[output truncated]"
        } else {
            ""
        };

        Ok(format!(
            "exit_code: {exit_code}\nstdout:\n{stdout}{stdout_suffix}\nstderr:\n{stderr}{stderr_suffix}"
        ))
    }

    fn build_argv(&self, arguments: &Value) -> Result<Vec<String>> {
        self.manifest
            .argv
            .iter()
            .map(|mapping| match (&mapping.literal, &mapping.from) {
                (Some(literal), None) => Ok(literal.clone()),
                (None, Some(name)) => scalar_argument(
                    arguments
                        .get(name)
                        .with_context(|| format!("argument {name:?} is missing"))?,
                ),
                _ => bail!("each argv mapping must specify exactly one of literal or from"),
            })
            .collect()
    }

    fn stdin_data(&self, arguments: &Value) -> Result<Option<Vec<u8>>> {
        self.manifest
            .stdin_from
            .as_ref()
            .map(|name| {
                arguments
                    .get(name)
                    .and_then(Value::as_str)
                    .map(|value| value.as_bytes().to_vec())
                    .with_context(|| format!("argument {name:?} must be a string"))
            })
            .transpose()
    }

    async fn run(
        &self,
        argv: &[String],
        stdin_data: Option<Vec<u8>>,
        environment: EnvironmentMode,
    ) -> Result<ProcessOutput> {
        let mut command = Command::new(&self.program);
        command.args(argv);
        if environment == EnvironmentMode::Clean {
            // Do not leak the agent's API keys or other environment secrets to tools.
            command.env_clear().env("PATH", COMMAND_PATH);
        }
        command
            .stdin(if stdin_data.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        let child = command
            .spawn()
            .with_context(|| format!("could not start {}", self.program.display()))?;
        let output = timeout(
            Duration::from_millis(self.manifest.timeout_ms),
            collect_output(child, stdin_data, self.manifest.max_output_bytes),
        )
        .await
        .map_err(|_| anyhow::anyhow!("tool timed out after {} ms", self.manifest.timeout_ms))??;

        Ok(output)
    }
}

struct ProcessOutput {
    status: std::process::ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    stdout_truncated: bool,
    stderr_truncated: bool,
}

async fn collect_output(
    mut child: Child,
    stdin_data: Option<Vec<u8>>,
    max_output_bytes: usize,
) -> Result<ProcessOutput> {
    let stdout = child
        .stdout
        .take()
        .context("stdout pipe was not available")?;
    let stderr = child
        .stderr
        .take()
        .context("stderr pipe was not available")?;
    let stdin = child.stdin.take();

    let (stdout, stderr, stdin_result, status) = tokio::join!(
        read_limited(stdout, max_output_bytes),
        read_limited(stderr, max_output_bytes),
        write_stdin(stdin, stdin_data),
        child.wait(),
    );

    stdin_result?;
    let status = status.context("could not wait for tool process")?;
    let (stdout, stdout_truncated) = stdout.context("could not read tool stdout")?;
    let (stderr, stderr_truncated) = stderr.context("could not read tool stderr")?;

    Ok(ProcessOutput {
        status,
        stdout,
        stderr,
        stdout_truncated,
        stderr_truncated,
    })
}

async fn read_limited<R>(mut reader: R, limit: usize) -> Result<(Vec<u8>, bool)>
where
    R: AsyncRead + Unpin,
{
    let mut output = Vec::with_capacity(limit.min(8192));
    let mut buffer = [0_u8; 8192];
    let mut truncated = false;

    loop {
        let read = reader.read(&mut buffer).await?;
        if read == 0 {
            break;
        }

        let remaining = limit.saturating_sub(output.len());
        let retained = read.min(remaining);
        output.extend_from_slice(&buffer[..retained]);
        truncated |= retained < read;
    }

    trim_incomplete_utf8(&mut output, truncated);

    Ok((output, truncated))
}

fn trim_incomplete_utf8(output: &mut Vec<u8>, truncated: bool) {
    if !truncated {
        return;
    }
    if let Err(error) = std::str::from_utf8(output) {
        if error.error_len().is_none() {
            output.truncate(error.valid_up_to());
        }
    }
}

async fn write_stdin(
    stdin: Option<tokio::process::ChildStdin>,
    data: Option<Vec<u8>>,
) -> Result<()> {
    match (stdin, data) {
        (Some(mut stdin), Some(data)) => stdin
            .write_all(&data)
            .await
            .context("could not write tool stdin"),
        _ => Ok(()),
    }
}

fn validate_manifest(manifest: &CommandManifest) -> Result<()> {
    if manifest.id.is_empty()
        || manifest.id.len() > 64
        || !manifest
            .id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        bail!("id must contain 1-64 ASCII letters, digits, underscores, or hyphens");
    }
    if manifest.version.trim().is_empty() {
        bail!("version must not be empty");
    }
    if manifest.description.trim().is_empty() {
        bail!("description must not be empty");
    }
    if !(1..=MAX_ALLOWED_TIMEOUT_MS).contains(&manifest.timeout_ms) {
        bail!("timeout_ms must be between 1 and {MAX_ALLOWED_TIMEOUT_MS}");
    }
    if !(1..=MAX_ALLOWED_OUTPUT_BYTES).contains(&manifest.max_output_bytes) {
        bail!("max_output_bytes must be between 1 and {MAX_ALLOWED_OUTPUT_BYTES}");
    }

    let schema = &manifest.input_schema;
    if schema.schema_type != "object" {
        bail!("input_schema.type must be 'object'");
    }
    for required in &schema.required {
        if !schema.properties.contains_key(required) {
            bail!("required property {required:?} is not declared in properties");
        }
    }
    for (name, property) in &schema.properties {
        if !matches!(
            property.schema_type.as_str(),
            "string" | "integer" | "number" | "boolean"
        ) {
            bail!(
                "property {name:?} has unsupported type {:?}",
                property.schema_type
            );
        }
    }

    for mapping in &manifest.argv {
        match (&mapping.literal, &mapping.from) {
            (Some(_), None) => {}
            (None, Some(name)) => validate_mapped_property(schema, name, false)?,
            _ => bail!("each argv mapping must specify exactly one of literal or from"),
        }
    }
    if let Some(name) = &manifest.stdin_from {
        validate_mapped_property(schema, name, true)?;
    }

    Ok(())
}

fn validate_mapped_property(schema: &InputSchema, name: &str, stdin: bool) -> Result<()> {
    let property = schema
        .properties
        .get(name)
        .with_context(|| format!("mapped property {name:?} is not declared in input_schema"))?;
    if !schema.required.iter().any(|required| required == name) {
        bail!("mapped property {name:?} must be required");
    }
    if stdin && property.schema_type != "string" {
        bail!("stdin_from property {name:?} must have type 'string'");
    }
    Ok(())
}

fn validate_arguments(schema: &InputSchema, arguments: &Value) -> Result<()> {
    let object = arguments
        .as_object()
        .context("tool arguments must be a JSON object")?;

    for required in &schema.required {
        if !object.contains_key(required) {
            bail!("required argument {required:?} is missing");
        }
    }

    for (name, value) in object {
        let Some(property) = schema.properties.get(name) else {
            if schema.additional_properties {
                continue;
            }
            bail!("unknown argument {name:?}");
        };

        let valid = match property.schema_type.as_str() {
            "string" => value.is_string(),
            "integer" => value.as_i64().is_some() || value.as_u64().is_some(),
            "number" => value.is_number(),
            "boolean" => value.is_boolean(),
            _ => false,
        };
        if !valid {
            bail!("argument {name:?} must be of type {}", property.schema_type);
        }
    }

    Ok(())
}

fn scalar_argument(value: &Value) -> Result<String> {
    match value {
        Value::String(value) => Ok(value.clone()),
        Value::Number(value) => Ok(value.to_string()),
        Value::Bool(value) => Ok(value.to_string()),
        _ => bail!("argv-mapped values must be strings, numbers, or booleans"),
    }
}

fn resolve_program(manifest_path: &Path, declared: &Path) -> Result<PathBuf> {
    let candidate = if declared.is_absolute() {
        declared.to_path_buf()
    } else {
        manifest_path
            .parent()
            .context("manifest has no parent directory")?
            .join(declared)
    };
    let program = candidate
        .canonicalize()
        .with_context(|| format!("could not resolve executable {}", candidate.display()))?;
    let metadata = std::fs::metadata(&program)
        .with_context(|| format!("could not inspect executable {}", program.display()))?;
    if !metadata.is_file() {
        bail!("executable {} is not a file", program.display());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o111 == 0 {
            bail!("executable {} has no execute permission", program.display());
        }
    }
    Ok(program)
}

#[cfg(test)]
mod tests {
    use super::{trim_incomplete_utf8, validate_arguments, InputSchema, PropertySchema};
    use serde_json::{json, Value};
    use std::collections::BTreeMap;

    fn string_schema() -> InputSchema {
        InputSchema {
            schema_type: "object".to_owned(),
            properties: BTreeMap::from([(
                "path".to_owned(),
                PropertySchema {
                    schema_type: "string".to_owned(),
                    description: None,
                },
            )]),
            required: vec!["path".to_owned()],
            additional_properties: false,
        }
    }

    #[test]
    fn argument_validation_accepts_matching_values() {
        assert!(validate_arguments(&string_schema(), &json!({"path":"notes.md"})).is_ok());
    }

    #[test]
    fn argument_validation_rejects_missing_wrong_and_extra_values() {
        let schema = string_schema();
        assert!(validate_arguments(&schema, &json!({})).is_err());
        assert!(validate_arguments(&schema, &json!({"path": 3})).is_err());
        assert!(validate_arguments(&schema, &json!({"path":"a", "extra":"b"})).is_err());
        assert!(validate_arguments(&schema, &Value::String("not an object".to_owned())).is_err());
    }

    #[test]
    fn truncated_output_does_not_split_a_utf8_character() {
        let mut bytes = vec![b'a', 0xd0, 0x9f, 0xd1];
        trim_incomplete_utf8(&mut bytes, true);

        assert_eq!(std::str::from_utf8(&bytes).unwrap(), "aП");
    }
}
