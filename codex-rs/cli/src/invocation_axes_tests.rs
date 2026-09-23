use super::InvocationAxes;
use super::invocation_axes_from_flags;
use super::prepare_invocation_axes;
use pretty_assertions::assert_eq;
use std::fs;
use std::path::Path;

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
        "model = \"not-copied\"\n\n[mcp_servers.alpha]\ncommand = \"echo\"\n",
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

    cleanup_runtime(&prepared_a.codex_home);
    cleanup_runtime(&prepared_b.codex_home);
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

    cleanup_runtime(&prepared.codex_home);
}

fn cleanup_runtime(codex_home: &Path) {
    for name in ["sessions", "archived_sessions", "skills"] {
        let path = codex_home.join(name);
        if path.exists() {
            let _ = fs::remove_file(&path);
        }
    }
    let _ = fs::remove_dir_all(codex_home);
}
