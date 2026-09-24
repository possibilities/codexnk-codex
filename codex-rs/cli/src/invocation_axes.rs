//! Process-launch split between credential identity, capability inputs, and
//! session history for `codex app-server`.
//!
//! Absent flags leave Codex on its normal `CODEX_HOME`. When all three are
//! present, this prepares a private runtime directory and points `CODEX_HOME`
//! at it. Sessions are linked from the history directory. Skills are copied
//! from the capabilities directory, MCP servers and the prompt append are
//! read from there, and `auth.json` is copied from the identity directory.
//! Project config is loaded under runtime-local trust; home-directory skills and
//! plugin marketplaces are excluded. CLI auth and MCP OAuth credentials stay
//! in files under the runtime home.

use anyhow::Context;
use std::collections::HashSet;
use std::fs;
use std::path::Path;
use std::path::PathBuf;

#[derive(Debug)]
pub struct InvocationAxes {
    pub identity: PathBuf,
    pub capabilities: PathBuf,
    pub history: PathBuf,
}

pub struct PreparedInvocation {
    pub codex_home: PathBuf,
}

pub fn invocation_axes_from_flags(
    identity: Option<PathBuf>,
    capabilities: Option<PathBuf>,
    history: Option<PathBuf>,
) -> anyhow::Result<Option<InvocationAxes>> {
    match (identity, capabilities, history) {
        (None, None, None) => Ok(None),
        (Some(identity), Some(capabilities), Some(history)) => Ok(Some(InvocationAxes {
            identity,
            capabilities,
            history,
        })),
        _ => anyhow::bail!("--identity, --capabilities, and --history-dir must be set together"),
    }
}

pub fn prepare_invocation_axes(axes: &InvocationAxes) -> anyhow::Result<PreparedInvocation> {
    let identity = existing_dir(&axes.identity, "--identity")?;
    let capabilities = existing_dir(&axes.capabilities, "--capabilities")?;
    let history = existing_dir(&axes.history, "--history-dir")?;

    let runtime = tempfile::TempDir::new().context("create invocation runtime directory")?;
    let codex_home = runtime.keep();

    fs::create_dir_all(history.join("sessions")).context("create history sessions directory")?;
    fs::create_dir_all(history.join("archived_sessions"))
        .context("create history archived sessions directory")?;
    symlink_dir(&history.join("sessions"), &codex_home.join("sessions"))?;
    symlink_dir(
        &history.join("archived_sessions"),
        &codex_home.join("archived_sessions"),
    )?;

    let capability_skills = capabilities.join("skills");
    let runtime_skills = codex_home.join("skills");
    if capability_skills.is_dir() {
        copy_tree(&capability_skills, &runtime_skills, &mut HashSet::new())?;
    } else if capability_skills.exists() {
        anyhow::bail!(
            "--capabilities skills path {} is not a directory",
            capability_skills.display()
        );
    } else {
        fs::create_dir(&runtime_skills).context("create empty runtime skills directory")?;
    }

    let auth_source = identity.join("auth.json");
    if auth_source.is_file() {
        fs::copy(&auth_source, codex_home.join("auth.json"))
            .with_context(|| format!("copy identity auth.json from {}", auth_source.display()))?;
    } else if auth_source.exists() {
        anyhow::bail!(
            "--identity auth.json at {} is not a file",
            auth_source.display()
        );
    }

    write_runtime_config(&capabilities, &codex_home)?;
    Ok(PreparedInvocation { codex_home })
}

/// Makes the current process load the prepared runtime as `CODEX_HOME`.
///
/// Call this once, before app-server startup reads the home directory.
pub fn apply_invocation_home(prepared: &PreparedInvocation) -> anyhow::Result<()> {
    let home = prepared
        .codex_home
        .to_str()
        .context("invocation runtime path is not valid Unicode")?;
    // SAFETY: app-server startup has not yet read `CODEX_HOME`. Rust marks this
    // unsafe because other threads may exist; this is the process launch input.
    unsafe { std::env::set_var("CODEX_HOME", home) };
    Ok(())
}

fn existing_dir(path: &Path, flag: &str) -> anyhow::Result<PathBuf> {
    let canonical = fs::canonicalize(path)
        .with_context(|| format!("{flag} path {} does not exist", path.display()))?;
    if !canonical.is_dir() {
        anyhow::bail!("{flag} path {} is not a directory", canonical.display());
    }
    Ok(canonical)
}

fn copy_tree(source: &Path, dest: &Path, ancestors: &mut HashSet<PathBuf>) -> anyhow::Result<()> {
    let canonical = fs::canonicalize(source)
        .with_context(|| format!("resolve skill directory {}", source.display()))?;
    anyhow::ensure!(
        ancestors.insert(canonical.clone()),
        "skill directory link cycle at {}",
        source.display()
    );
    fs::create_dir(dest).with_context(|| format!("create {}", dest.display()))?;
    for entry in fs::read_dir(source).with_context(|| format!("read {}", source.display()))? {
        let entry = entry?;
        let from = entry.path();
        let to = dest.join(entry.file_name());
        let metadata = fs::metadata(&from).with_context(|| format!("stat {}", from.display()))?;
        if metadata.is_dir() {
            copy_tree(&from, &to, ancestors)?;
        } else {
            fs::copy(&from, &to)
                .with_context(|| format!("copy {} to {}", from.display(), to.display()))?;
        }
    }
    ancestors.remove(&canonical);
    Ok(())
}

fn symlink_dir(target: &Path, link: &Path) -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, link)
            .with_context(|| format!("link {} to {}", link.display(), target.display()))?;
    }
    #[cfg(windows)]
    {
        std::os::windows::fs::symlink_dir(target, link)
            .with_context(|| format!("link {} to {}", link.display(), target.display()))?;
    }
    Ok(())
}

fn write_runtime_config(capabilities: &Path, codex_home: &Path) -> anyhow::Result<()> {
    let mut table = toml::map::Map::new();
    table.insert(
        "cli_auth_credentials_store".to_string(),
        toml::Value::String("file".to_string()),
    );
    table.insert(
        "mcp_oauth_credentials_store".to_string(),
        toml::Value::String("file".to_string()),
    );

    let config_path = capabilities.join("config.toml");
    if config_path.is_file() {
        let text = fs::read_to_string(&config_path)
            .with_context(|| format!("read {}", config_path.display()))?;
        let parsed: toml::Value =
            toml::from_str(&text).with_context(|| format!("parse {}", config_path.display()))?;
        if let Some(mcp_servers) = parsed.get("mcp_servers").cloned() {
            table.insert("mcp_servers".to_string(), mcp_servers);
        }
        if let Some(projects) = parsed.get("projects").cloned() {
            table.insert("projects".to_string(), projects);
        }
    }

    let append_path = capabilities.join("SYSTEM_APPEND.md");
    if append_path.is_file() {
        let text = fs::read_to_string(&append_path)
            .with_context(|| format!("read {}", append_path.display()))?;
        table.insert(
            "developer_instructions".to_string(),
            toml::Value::String(text),
        );
    }

    let rendered = toml::to_string_pretty(&toml::Value::Table(table))
        .context("render invocation config.toml")?;
    fs::write(codex_home.join("config.toml"), rendered).context("write invocation config.toml")?;
    Ok(())
}

#[cfg(test)]
#[path = "invocation_axes_tests.rs"]
mod tests;
