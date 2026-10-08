use super::InvocationAxes;
use super::invocation_axes_from_flags;
use super::prepare_invocation_axes;
use pretty_assertions::assert_eq;
use std::fs;
use std::path::Path;

#[tokio::test]
async fn app_server_flags_load_selected_and_project_skills_without_prior_codex_home()
-> anyhow::Result<()> {
    use app_test_support::TestAppServer;
    use codex_app_server_protocol::ClientRequest;
    use codex_app_server_protocol::ConfigReadParams;
    use codex_app_server_protocol::ConfigReadResponse;
    use codex_app_server_protocol::SkillsListParams;
    use codex_app_server_protocol::SkillsListResponse;

    let original_home = tempfile::tempdir()?;
    let identity = tempfile::tempdir()?;
    let capabilities = tempfile::tempdir()?;
    let history = tempfile::tempdir()?;
    let project = tempfile::tempdir()?;
    let identity_auth = br#"{"OPENAI_API_KEY":"scoped-identity-key"}"#;
    fs::write(identity.path().join("auth.json"), identity_auth)?;
    fs::create_dir_all(project.path().join(".git"))?;
    let project_key =
        toml::Value::String(project.path().to_string_lossy().into_owned()).to_string();
    fs::write(
        capabilities.path().join("config.toml"),
        format!(
            "[projects.{project_key}]\ntrust_level = 'trusted'\n\
             [mcp_servers.selected]\ncommand = 'selected-server'\nenabled = false\n"
        ),
    )?;
    fs::write(
        original_home.path().join("config.toml"),
        "[mcp_servers.ambient]\ncommand = 'ambient-server'\nenabled = false\n",
    )?;
    fs::create_dir_all(project.path().join(".codex"))?;
    fs::write(
        project.path().join(".codex/config.toml"),
        "[mcp_servers.project]\ncommand = 'project-server'\nenabled = false\n",
    )?;
    for (root, name) in [
        (original_home.path().join("skills"), "ambient-skill"),
        (capabilities.path().join("skills"), "selected-skill"),
        (project.path().join(".agents/skills"), "project-agent-skill"),
        (project.path().join(".codex/skills"), "project-codex-skill"),
    ] {
        let skill_dir = root.join(name);
        fs::create_dir_all(&skill_dir)?;
        fs::write(
            skill_dir.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: {name} description\n---\n"),
        )?;
    }

    let codex = codex_utils_cargo_bin::cargo_bin("codex")?;
    let mut app_server = TestAppServer::builder()
        .with_program(&codex)
        .with_codex_home(original_home.path())
        .without_auto_env()
        .with_env_overrides(&[("OPENAI_API_KEY", None), ("CODEX_API_KEY", None)])
        .with_plugin_startup_tasks()
        .with_args(&[
            "app-server",
            "--identity",
            identity.path().to_str().expect("identity path"),
            "--capabilities",
            capabilities.path().to_str().expect("capabilities path"),
            "--history-dir",
            history.path().to_str().expect("history path"),
            "-c",
            "cli_auth_credentials_store='ephemeral'",
            "-c",
            "mcp_oauth_credentials_store='keyring'",
        ])
        .build_initialized()
        .await?;
    let request_id = app_server
        .send_skills_list_request(SkillsListParams {
            cwds: vec![project.path().to_path_buf()],
            force_reload: true,
        })
        .await?;
    let SkillsListResponse { data } = app_server.read_response(request_id).await?;
    assert_eq!(data.len(), 1);
    assert_eq!(data[0].errors, Vec::new());
    for name in [
        "selected-skill",
        "project-agent-skill",
        "project-codex-skill",
    ] {
        assert!(data[0].skills.iter().any(|skill| skill.name == name));
    }
    assert!(
        data[0]
            .skills
            .iter()
            .all(|skill| skill.name != "ambient-skill")
    );

    let config: ConfigReadResponse = app_server
        .request(|request_id| ClientRequest::ConfigRead {
            request_id,
            params: ConfigReadParams {
                include_layers: true,
                cwd: Some(project.path().to_string_lossy().into_owned()),
            },
        })
        .await?;
    let mcp_names = config
        .layers
        .expect("config layers")
        .into_iter()
        .filter_map(|layer| layer.config.get("mcp_servers").cloned())
        .flat_map(|servers| {
            servers
                .as_object()
                .expect("MCP server table")
                .keys()
                .cloned()
                .collect::<Vec<_>>()
        })
        .collect::<std::collections::HashSet<_>>();
    assert!(mcp_names.contains("selected"));
    assert!(mcp_names.contains("project"));
    assert!(!mcp_names.contains("ambient"));

    let selected = data[0]
        .skills
        .iter()
        .find(|skill| skill.name == "selected-skill")
        .expect("selected skill");
    let selected_path = selected
        .path
        .to_inferred_abs_path()
        .expect("host skill path");
    let runtime_home = selected_path
        .as_path()
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .expect("runtime home")
        .to_path_buf();
    let request_id = app_server
        .send_raw_request(
            "account/read",
            Some(serde_json::json!({"refreshToken": false})),
        )
        .await?;
    let account: serde_json::Value = app_server.read_response(request_id).await?;
    assert_eq!(account["account"], serde_json::json!({"type": "apiKey"}));
    let request_id = app_server.send_logout_account_request().await?;
    let _: codex_app_server_protocol::LogoutAccountResponse =
        app_server.read_response(request_id).await?;
    assert!(!runtime_home.join("auth.json").exists());
    assert_eq!(fs::read(identity.path().join("auth.json"))?, identity_auth);
    let exit = app_server.shutdown_gracefully().await?;
    assert!(exit.success());
    assert!(
        !runtime_home.exists(),
        "process exit removes its private runtime"
    );
    assert!(history.path().join("sessions").is_dir());
    assert!(history.path().join("archived_sessions").is_dir());
    Ok(())
}

#[test]
fn flags_must_be_all_present_or_all_absent() {
    assert!(
        invocation_axes_from_flags(None, None, None)
            .unwrap()
            .is_none()
    );
    let err = invocation_axes_from_flags(Some("id".into()), None, Some("hist".into()))
        .expect_err("partial flags");
    assert!(err.to_string().contains("must be set together"));
}

#[test]
fn axes_share_history_and_keep_identity_and_capabilities_apart() {
    let identity_a = tempfile::tempdir().expect("identity a");
    let identity_b = tempfile::tempdir().expect("identity b");
    let capabilities_a = tempfile::tempdir().expect("capabilities a");
    let capabilities_b = tempfile::tempdir().expect("capabilities b");
    let history = tempfile::tempdir().expect("history");

    fs::write(identity_a.path().join("auth.json"), "{\"token\":\"alpha\"}").expect("auth a");
    fs::write(identity_b.path().join("auth.json"), "{\"token\":\"beta\"}").expect("auth b");
    fs::write(
        capabilities_a.path().join("config.toml"),
        "model = \"not-copied\"\n\n[mcp_servers.alpha]\ncommand = \"echo\"\n\n[projects.\"/repo\"]\ntrust_level = \"trusted\"\n",
    )
    .expect("config a");
    fs::write(
        capabilities_a.path().join("SYSTEM_APPEND.md"),
        "alpha prompt\n",
    )
    .expect("append a");
    fs::create_dir(capabilities_a.path().join("skills")).expect("skills a");
    fs::write(
        capabilities_a.path().join("skills").join("from-a.txt"),
        "skill-a",
    )
    .expect("skill file");
    fs::write(
        capabilities_b.path().join("SYSTEM_APPEND.md"),
        "beta prompt\n",
    )
    .expect("append b");

    let prepared_a = prepare_invocation_axes(&InvocationAxes {
        identity: identity_a.path().to_path_buf(),
        capabilities: capabilities_a.path().to_path_buf(),
        history: history.path().to_path_buf(),
    })
    .expect("prepare a");
    let prepared_b = prepare_invocation_axes(&InvocationAxes {
        identity: identity_b.path().to_path_buf(),
        capabilities: capabilities_b.path().to_path_buf(),
        history: history.path().to_path_buf(),
    })
    .expect("prepare b");

    let sessions_a = fs::canonicalize(prepared_a.codex_home.join("sessions")).expect("sessions a");
    let sessions_b = fs::canonicalize(prepared_b.codex_home.join("sessions")).expect("sessions b");
    let shared_sessions = fs::canonicalize(history.path().join("sessions")).expect("shared");
    assert_eq!(sessions_a, shared_sessions);
    assert_eq!(sessions_b, shared_sessions);

    fs::write(sessions_a.join("thread.txt"), "same-history").expect("write session");
    assert_eq!(
        fs::read_to_string(sessions_b.join("thread.txt")).expect("read other runtime"),
        "same-history"
    );

    assert_eq!(
        fs::read(identity_a.path().join("auth.json")).expect("identity unchanged"),
        b"{\"token\":\"alpha\"}"
    );
    assert_eq!(
        fs::read(prepared_a.codex_home.join("auth.json")).expect("runtime auth a"),
        b"{\"token\":\"alpha\"}"
    );
    assert_eq!(
        fs::read(prepared_b.codex_home.join("auth.json")).expect("runtime auth b"),
        b"{\"token\":\"beta\"}"
    );

    let config_a = fs::read_to_string(prepared_a.codex_home.join("config.toml")).expect("config a");
    assert!(config_a.contains("alpha prompt"));
    assert!(config_a.contains("[mcp_servers.alpha]"));
    assert!(config_a.contains("[projects.\"/repo\"]"));
    assert!(config_a.contains("cli_auth_credentials_store = \"file\""));
    assert!(config_a.contains("mcp_oauth_credentials_store = \"file\""));
    assert!(!config_a.contains("not-copied"));
    let config_b = fs::read_to_string(prepared_b.codex_home.join("config.toml")).expect("config b");
    assert!(config_b.contains("beta prompt"));
    assert!(!config_b.contains("mcp_servers"));

    let runtime_skill = prepared_a.codex_home.join("skills").join("from-a.txt");
    assert!(
        !runtime_skill
            .symlink_metadata()
            .expect("skill metadata")
            .file_type()
            .is_symlink()
    );
    assert_eq!(
        fs::read_to_string(&runtime_skill).expect("skill"),
        "skill-a"
    );
    fs::write(
        prepared_a
            .codex_home
            .join("skills")
            .join("runtime-only.txt"),
        "no-writeback",
    )
    .expect("write runtime skill");
    assert!(
        !capabilities_a
            .path()
            .join("skills")
            .join("runtime-only.txt")
            .exists()
    );
    assert!(
        !prepared_b
            .codex_home
            .join("skills")
            .join("from-a.txt")
            .exists()
    );

    let runtime_a = prepared_a.codex_home.clone();
    let runtime_b = prepared_b.codex_home.clone();
    drop(prepared_a);
    assert!(
        !runtime_a.exists(),
        "dropping the owner removes copied credentials"
    );
    assert_eq!(
        fs::read(runtime_b.join("auth.json")).expect("other runtime remains live"),
        b"{\"token\":\"beta\"}"
    );
    drop(prepared_b);
    assert!(!runtime_b.exists());
    assert_eq!(
        fs::read_to_string(shared_sessions.join("thread.txt")).expect("history survives"),
        "same-history"
    );
    assert_eq!(
        fs::read(identity_a.path().join("auth.json")).expect("identity survives"),
        b"{\"token\":\"alpha\"}"
    );
}

#[cfg(unix)]
#[test]
fn linked_skills_are_copied_without_retaining_links_to_capabilities() {
    use std::os::unix::fs::symlink;

    let identity = tempfile::tempdir().expect("identity");
    let capabilities = tempfile::tempdir().expect("capabilities");
    let history = tempfile::tempdir().expect("history");
    let source = tempfile::tempdir().expect("skill source");
    let source_skill = source.path().join("shared");
    fs::create_dir(&source_skill).expect("source skill directory");
    fs::write(source_skill.join("SKILL.md"), "original").expect("source skill");
    let skills = capabilities.path().join("skills");
    fs::create_dir(&skills).expect("skills directory");
    symlink(&source_skill, skills.join("linked-skill")).expect("linked skill directory");

    let prepared = prepare_invocation_axes(&InvocationAxes {
        identity: identity.path().to_path_buf(),
        capabilities: capabilities.path().to_path_buf(),
        history: history.path().to_path_buf(),
    })
    .expect("prepare");
    let copied_skill = prepared.codex_home.join("skills/linked-skill/SKILL.md");
    assert_eq!(
        fs::read_to_string(&copied_skill).expect("copied skill"),
        "original"
    );
    assert!(
        !prepared
            .codex_home
            .join("skills/linked-skill")
            .symlink_metadata()
            .expect("copied skill metadata")
            .file_type()
            .is_symlink()
    );

    fs::write(source_skill.join("SKILL.md"), "changed at source").expect("update source");
    assert_eq!(
        fs::read_to_string(&copied_skill).expect("stable copy"),
        "original"
    );
    fs::write(&copied_skill, "changed at runtime").expect("update runtime");
    assert_eq!(
        fs::read_to_string(source_skill.join("SKILL.md")).expect("stable source"),
        "changed at source"
    );

    drop(prepared);
}

#[tokio::test]
async fn invocation_axes_failed_preparation_removes_copied_credentials() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let identity = root.path().join("identity");
    let capabilities = root.path().join("capabilities");
    let history = root.path().join("history");
    let runtime = root.path().join("runtime");
    for path in [&identity, &capabilities, &history, &runtime] {
        fs::create_dir(path)?;
    }
    let auth = br#"{"OPENAI_API_KEY":"failed-preparation-fixture-only"}"#;
    fs::write(identity.join("auth.json"), auth)?;
    // Config parsing happens after the identity file is copied into the runtime.
    fs::write(capabilities.join("config.toml"), "[invalid TOML")?;
    let codex = codex_utils_cargo_bin::cargo_bin("codex")?;
    let result = tokio::process::Command::new(codex)
        .arg("app-server")
        .arg("--identity")
        .arg(&identity)
        .arg("--capabilities")
        .arg(&capabilities)
        .arg("--history-dir")
        .arg(&history)
        .env("TMPDIR", &runtime)
        .env("TMP", &runtime)
        .env("TEMP", &runtime)
        .env_remove("OPENAI_API_KEY")
        .env_remove("CODEX_API_KEY")
        .current_dir(root.path())
        .output()
        .await?;
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("parse"));
    assert_eq!(
        fs::read_dir(&runtime)?.count(),
        0,
        "failed preparation leaves no copied credentials"
    );
    assert_eq!(fs::read(identity.join("auth.json"))?, auth);
    assert!(history.join("sessions").is_dir());
    assert!(history.join("archived_sessions").is_dir());
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn invocation_axes_stdio_watchdog_removes_copied_credentials() -> anyhow::Result<()> {
    use anyhow::Context;
    use std::process::Stdio;
    use tokio::io::AsyncBufReadExt;
    use tokio::io::AsyncWriteExt;
    use tokio::io::BufReader;
    use tokio::time::Duration;
    use tokio::time::Instant;
    use tokio::time::timeout;

    let root = tempfile::tempdir()?;
    let identity = root.path().join("identity");
    let capabilities = root.path().join("capabilities");
    let history = root.path().join("history");
    let runtime = root.path().join("runtime");
    for path in [&identity, &capabilities, &history, &runtime] {
        fs::create_dir(path)?;
    }
    let auth = br#"{"OPENAI_API_KEY":"watchdog-fixture-only"}"#;
    fs::write(identity.join("auth.json"), auth)?;
    let mut process = tokio::process::Command::new(codex_utils_cargo_bin::cargo_bin("codex")?)
        .arg("app-server")
        .arg("--identity")
        .arg(&identity)
        .arg("--capabilities")
        .arg(&capabilities)
        .arg("--history-dir")
        .arg(&history)
        .env("TMPDIR", &runtime)
        .env("TMP", &runtime)
        .env("TEMP", &runtime)
        .env("RUST_LOG", "codex_app_server_transport=error")
        .env("HOME", root.path())
        .env_remove("OPENAI_API_KEY")
        .env_remove("CODEX_API_KEY")
        .current_dir(root.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        // Leave stderr unread to force the real shutdown watchdog, not normal teardown.
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;
    let mut stdin = process.stdin.take().context("missing stdin")?;
    let mut stdout = BufReader::new(process.stdout.take().context("missing stdout")?).lines();
    let initialize = serde_json::json!({
        "id": 1, "method": "initialize",
        "params": {"clientInfo": {"name": "invocation-watchdog-test", "version": "1"}}
    });
    stdin
        .write_all(format!("{initialize}\n").as_bytes())
        .await?;
    let response = timeout(Duration::from_secs(10), stdout.next_line())
        .await??
        .context("app-server did not initialize")?;
    let response: serde_json::Value = serde_json::from_str(&response)?;
    assert!(response.get("result").is_some(), "{response}");
    let runtime_home = fs::read_dir(&runtime)?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .find(|path| path.join("auth.json").is_file())
        .context("missing copied credentials")?;
    assert_eq!(fs::read(runtime_home.join("auth.json"))?, auth);
    fs::write(
        history.join("sessions/watchdog-sentinel"),
        "history survives",
    )?;
    assert!(
        timeout(
            Duration::from_secs(2),
            stdin.write_all(&b"{\n".repeat(1024 * 1024))
        )
        .await
        .is_err(),
        "expected stderr to block transport"
    );
    let signalled_at = Instant::now();
    let status = std::process::Command::new("kill")
        .arg("-TERM")
        .arg(process.id().context("missing pid")?.to_string())
        .status()?;
    assert!(status.success());
    let status = timeout(Duration::from_secs(55), process.wait()).await??;
    assert_eq!(status.code(), Some(1));
    assert!(signalled_at.elapsed() >= Duration::from_secs(45));
    assert!(
        !runtime_home.exists(),
        "watchdog removes copied credentials"
    );
    assert_eq!(fs::read(identity.join("auth.json"))?, auth);
    assert_eq!(
        fs::read_to_string(history.join("sessions/watchdog-sentinel"))?,
        "history survives"
    );
    Ok(())
}
