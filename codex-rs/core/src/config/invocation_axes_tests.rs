use super::*;
use codex_config::LoaderOverrides;
use pretty_assertions::assert_eq;

#[tokio::test]
async fn invocation_axes_force_file_credentials_without_relying_on_user_config() {
    let home = tempfile::tempdir().unwrap();
    for ignore_user_config in [false, true] {
        let config = ConfigBuilder::default()
            .codex_home(home.path().to_path_buf())
            .fallback_cwd(Some(home.path().to_path_buf()))
            .loader_overrides(LoaderOverrides {
                exclude_home_capabilities: true,
                ignore_user_config,
                ..LoaderOverrides::without_managed_config_for_tests()
            })
            .cli_overrides(vec![
                ("cli_auth_credentials_store".into(), "ephemeral".into()),
                ("mcp_oauth_credentials_store".into(), "keyring".into()),
            ])
            .build()
            .await
            .unwrap();
        assert_eq!(
            (
                config.cli_auth_credentials_store_mode,
                config.mcp_oauth_credentials_store_mode
            ),
            (
                AuthCredentialsStoreMode::File,
                OAuthCredentialsStoreMode::File
            )
        );
    }
}

#[tokio::test]
async fn invocation_axes_refuse_incompatible_managed_credential_storage() {
    let home = tempfile::tempdir().unwrap();
    let requirements = home.path().join("requirements.toml");
    std::fs::write(&requirements, "cli_auth_credentials_store = 'keyring'").unwrap();
    let error = ConfigBuilder::default()
        .codex_home(home.path().to_path_buf())
        .fallback_cwd(Some(home.path().to_path_buf()))
        .loader_overrides(LoaderOverrides {
            exclude_home_capabilities: true,
            system_requirements_path: Some(requirements),
            ..LoaderOverrides::without_managed_config_for_tests()
        })
        .build()
        .await
        .unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
    assert!(
        error
            .to_string()
            .contains("managed cli_auth_credentials_store")
    );
}

#[tokio::test]
async fn invocation_axes_reject_only_selected_gateway_oauth_providers() {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(
        home.path().join("config.toml"),
        r#"
[model_providers.gateway]
name = "Test gateway"
base_url = "https://gateway.example/v1"
wire_api = "responses"
[model_providers.gateway.gateway_oauth]
authorization_url = "https://gateway.example/authorize"
token_url = "https://gateway.example/token"
client_id = "test-client"
delivery = { kind = "header", name = "X-Gateway-Authorization" }
"#,
    )
    .unwrap();
    for (scoped, provider) in [(true, "openai"), (false, "gateway"), (true, "gateway")] {
        let result = ConfigBuilder::default()
            .codex_home(home.path().to_path_buf())
            .fallback_cwd(Some(home.path().to_path_buf()))
            .loader_overrides(LoaderOverrides {
                exclude_home_capabilities: scoped,
                ..LoaderOverrides::without_managed_config_for_tests()
            })
            .cli_overrides(vec![("model_provider".into(), provider.into())])
            .build()
            .await;
        if scoped && provider == "gateway" {
            let error = result.unwrap_err();
            assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
            assert!(
                error
                    .to_string()
                    .contains("gateway OAuth is unavailable with invocation axes")
            );
        } else {
            assert_eq!(result.unwrap().model_provider_id, provider);
        }
    }
}
