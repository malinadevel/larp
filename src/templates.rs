use std::{
    fs::{self, File, OpenOptions},
    io,
    path::{Path, PathBuf},
};

use anyhow::{bail, Context, Result};

pub fn install_defaults(destination: &Path) -> Result<()> {
    let source = template_directory()?;
    copy_missing_tree(&source, destination)
}

pub(crate) fn template_directory() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("LARP_TEMPLATES_DIR") {
        let path = PathBuf::from(path);
        if path.is_dir() {
            return Ok(path);
        }
        bail!("LARP_TEMPLATES_DIR is not a directory: {}", path.display());
    }

    let executable = std::env::current_exe().context("could not locate the larp executable")?;
    if let Some(directory) = executable.parent() {
        for candidate in [
            directory.join("../share/larp/templates"),
            directory.join("templates"),
        ] {
            if candidate.is_dir() {
                return candidate
                    .canonicalize()
                    .with_context(|| format!("could not resolve {}", candidate.display()));
            }
        }
    }

    let source_tree = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("templates");
    if source_tree.is_dir() {
        return Ok(source_tree);
    }

    bail!(
        "could not find templates; set LARP_TEMPLATES_DIR or install them under share/larp/templates"
    )
}

pub(crate) fn copy_missing_tree(source: &Path, destination: &Path) -> Result<()> {
    fs::create_dir_all(destination)
        .with_context(|| format!("could not create {}", destination.display()))?;

    let mut entries = fs::read_dir(source)
        .with_context(|| format!("could not read templates directory {}", source.display()))?
        .map(|entry| {
            entry
                .map(|entry| entry.path())
                .with_context(|| format!("could not read an entry in {}", source.display()))
        })
        .collect::<Result<Vec<_>>>()?;
    entries.sort();

    for source_path in entries {
        let file_type = fs::symlink_metadata(&source_path)
            .with_context(|| format!("could not inspect {}", source_path.display()))?
            .file_type();
        let destination_path = destination.join(
            source_path
                .file_name()
                .context("template entry has no file name")?,
        );

        if file_type.is_dir() {
            copy_missing_tree(&source_path, &destination_path)?;
        } else if file_type.is_file() {
            copy_missing_file(&source_path, &destination_path)?;
        } else {
            bail!("unsupported template entry: {}", source_path.display());
        }
    }

    Ok(())
}

fn copy_missing_file(source: &Path, destination: &Path) -> Result<()> {
    let mut output = match OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)
    {
        Ok(output) => output,
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            let metadata = fs::metadata(destination)
                .with_context(|| format!("could not inspect {}", destination.display()))?;
            if metadata.is_file() {
                return Ok(());
            }
            bail!(
                "template destination is not a file: {}",
                destination.display()
            );
        }
        Err(error) => {
            return Err(error)
                .with_context(|| format!("could not create {}", destination.display()));
        }
    };

    let copy_result = (|| -> Result<()> {
        let mut input = File::open(source)
            .with_context(|| format!("could not open template {}", source.display()))?;
        io::copy(&mut input, &mut output)
            .with_context(|| format!("could not copy template to {}", destination.display()))?;
        output
            .sync_all()
            .with_context(|| format!("could not sync {}", destination.display()))?;
        let permissions = fs::metadata(source)
            .with_context(|| format!("could not inspect template {}", source.display()))?
            .permissions();
        fs::set_permissions(destination, permissions).with_context(|| {
            format!(
                "could not preserve permissions on {}",
                destination.display()
            )
        })?;
        Ok(())
    })();

    if copy_result.is_err() {
        let _ = fs::remove_file(destination);
    }
    copy_result
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::PathBuf,
        sync::atomic::{AtomicUsize, Ordering},
    };

    use super::{copy_missing_tree, install_defaults};

    static NEXT_DIRECTORY: AtomicUsize = AtomicUsize::new(0);

    fn temporary_directory() -> PathBuf {
        let sequence = NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("larp-templates-{}-{sequence}", std::process::id()));
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn copies_missing_templates_without_overwriting_user_edits() {
        let root = temporary_directory();
        let source = root.join("source");
        let destination = root.join("destination");
        fs::create_dir_all(source.join("agents/default/tools")).unwrap();
        fs::write(source.join("config.toml"), "model = 'test'\n").unwrap();
        fs::write(
            source.join("agents/default/tools/bash.toml"),
            "id = 'bash'\n",
        )
        .unwrap();
        fs::create_dir_all(&destination).unwrap();
        fs::write(destination.join("config.toml"), "model = 'user-choice'\n").unwrap();

        copy_missing_tree(&source, &destination).unwrap();

        assert_eq!(
            fs::read_to_string(destination.join("config.toml")).unwrap(),
            "model = 'user-choice'\n"
        );
        assert_eq!(
            fs::read_to_string(destination.join("agents/default/tools/bash.toml")).unwrap(),
            "id = 'bash'\n"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn startup_templates_include_policy_prompt_and_command_manifests() {
        let destination = temporary_directory();
        install_defaults(&destination).unwrap();

        for relative_path in [
            "agents/index.toml",
            "agents/default/config.toml",
            "agents/default/security.toml",
            "agents/default/AGENT_PROMPT.md",
            "SYSTEM_PROMPT.md",
            "agents/default/tools/bash.toml",
            "agents/default/tools/read_file.toml",
        ] {
            assert!(destination.join(relative_path).is_file(), "{relative_path}");
        }

        fs::remove_dir_all(destination).unwrap();
    }
}
