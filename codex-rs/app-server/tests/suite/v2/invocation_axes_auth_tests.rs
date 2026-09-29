//! Scoped account operations must never enter enterprise keyring storage.

use super::*;
use pretty_assertions::assert_eq;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn invocation_axes_reject_enterprise_login_and_logout_without_keyring() -> Result<()> {
    const CHILD: &str = "CODEX_INVOCATION_AXES_AUTH_TEST_CHILD";
    if std::env::var_os(CHILD).is_none() {
        // Isolate the credential builder and process home from other API tests.
        let home = TempDir::new()?;
        let oauth_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/oauth/revoke"))
            .respond_with(ResponseTemplate::new(200))
            .expect(2)
            .mount(&oauth_server)
            .await;
        let output = Command::new(std::env::current_exe()?)
            .args([
                "--exact",
                "suite::v2::account::enterprise_tests::invocation_axes::invocation_axes_reject_enterprise_login_and_logout_without_keyring",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .env("CODEX_HOME", home.path())
            .env(
                REFRESH_TOKEN_URL_OVERRIDE_ENV_VAR,
                format!("{}/oauth/token", oauth_server.uri()),
            )
            .env_remove("OPENAI_API_KEY")
            .env_remove("CODEX_API_KEY")
            .env_remove(CODEX_ACCESS_TOKEN_ENV_VAR)
            .current_dir(home.path())
            .output()
            .await?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success() && stdout.contains("1 passed; 0 failed"),
            "{stdout}\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        return Ok(());
    }

    let keyring_attempts = Arc::new(AtomicUsize::new(0));
    keyring::set_default_credential_builder(Box::new(CountingKeyring(Arc::clone(
        &keyring_attempts,
    ))));
    let mut outcomes = Vec::new();
    for xaa_enabled in [false, true] {
        let server = MockServer::start().await;
        let origin = server.uri();
        Mock::given(method("GET"))
            .and(path("/backend-api/wham/config/bundle"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(CloudConfigBundleFixture::default().into_bundle()),
            )
            .mount(&server)
            .await;
        let home = TempDir::new()?;
        let feature_config = if xaa_enabled {
            MockResponsesConfig::new(&origin).enable_feature(Feature::UseXaa)
        } else {
            MockResponsesConfig::new(&origin).disable_feature(Feature::UseXaa)
        };
        feature_config
            .disable_feature(Feature::SecretAuthStorage)
            .disable_feature(Feature::Apps)
            .with_root_config(&format!(
                r#"cli_auth_credentials_store = "file"
mcp_oauth_credentials_store = "file"
chatgpt_base_url = "{origin}/backend-api"
analytics = {{ enabled = false }}"#
            ))
            .with_provider_config("requires_openai_auth = true")
            .with_extra_config(&format!(
                r#"[mcp_enterprise_managed_auth.idp]
issuer = "{origin}/idp"
client_id = "idp-client"

[mcp_servers.enterprise]
url = "{origin}/mcp"
auth = "ema_auth"
oauth = {{ client_id = "mcp-client", authorization_server_issuer = "{origin}/as" }}"#
            ))
            .write(home.path())?;
        write_chatgpt_auth(
            home.path(),
            ChatGptAuthFixture::new("account-access")
                .refresh_token("account-refresh")
                .account_id(WORKSPACE_ID_INITIAL)
                .chatgpt_user_id("enterprise-user")
                .plan_type("business"),
            AuthCredentialsStoreMode::File,
        )?;
        let loader_overrides = LoaderOverrides {
            exclude_home_capabilities: true,
            ..LoaderOverrides::without_managed_config_for_tests()
        };
        let config = Arc::new(
            ConfigBuilder::default()
                .codex_home(home.path().to_path_buf())
                .fallback_cwd(Some(home.path().to_path_buf()))
                .loader_overrides(loader_overrides.clone())
                .build()
                .await?,
        );
        let client = in_process::start(InProcessStartArgs {
            arg0_paths: Arg0DispatchPaths::default(),
            config,
            cli_overrides: Vec::new(),
            loader_overrides,
            strict_config: false,
            cloud_config_bundle: CloudConfigBundleLoader::default(),
            thread_config_loader: Arc::new(NoopThreadConfigLoader),
            feedback: CodexFeedback::new(),
            log_db: None,
            state_db: None,
            environment_manager: Arc::new(EnvironmentManager::default_for_tests()),
            config_warnings: Vec::new(),
            embedded_network_policy: Default::default(),
            session_source: SessionSource::Cli,
            enable_codex_api_key_env: false,
            initialize: InitializeParams {
                client_info: ClientInfo {
                    name: "codex-invocation-axes-tests".into(),
                    title: None,
                    version: "0.1.0".into(),
                },
                capabilities: Some(InitializeCapabilities {
                    experimental_api: true,
                    ..Default::default()
                }),
            },
            channel_capacity: in_process::DEFAULT_IN_PROCESS_CHANNEL_CAPACITY,
        })
        .await?;
        let thread = enterprise_rpc(&client, "thread/start", json!({})).await?;
        let thread_id = thread["thread"]["id"].as_str().context("thread ID")?;
        let login_error = enterprise_rpc(
            &client,
            "mcpServer/oauth/login",
            json!({"name":"enterprise","threadId":thread_id}),
        )
        .await
        .expect_err("scoped enterprise login must be refused")
        .to_string();
        enterprise_rpc(&client, "config/mcpServer/reload", json!(null)).await?;
        let reloaded_error = enterprise_rpc(
            &client,
            "mcpServer/oauth/login",
            json!({"name":"enterprise","threadId":thread_id}),
        )
        .await
        .expect_err("reload must preserve scoped enterprise refusal")
        .to_string();
        let logout = enterprise_rpc(&client, "account/logout", json!(null)).await?;
        client.shutdown().await?;
        outcomes.push((
            login_error,
            reloaded_error,
            logout,
            home.path().join("auth.json").exists(),
            keyring_attempts.swap(0, Ordering::SeqCst),
        ));
    }
    let denial = "mcpServer/oauth/login: enterprise MCP authentication is unavailable with invocation axes because it requires OS keyring storage".to_string();
    assert_eq!(
        outcomes,
        vec![(denial.clone(), denial, json!({}), false, 0); 2]
    );
    Ok(())
}

struct CountingKeyring(Arc<AtomicUsize>);

impl CredentialBuilderApi for CountingKeyring {
    fn build(
        &self,
        _target: Option<&str>,
        _service: &str,
        _user: &str,
    ) -> keyring::Result<Box<Credential>> {
        self.0.fetch_add(1, Ordering::SeqCst);
        // Count even failed attempts; no platform credential is ever constructed.
        Err(keyring::Error::NoEntry)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn persistence(&self) -> CredentialPersistence {
        CredentialPersistence::ProcessOnly
    }
}
