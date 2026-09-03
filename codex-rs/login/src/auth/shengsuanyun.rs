use std::path::Path;

use codex_config::types::AuthCredentialsStoreMode;
use codex_protocol::auth::AuthMode;
use serde::Deserialize;
use serde::Serialize;

use super::manager::save_auth;
use super::storage::AuthDotJson;
use super::storage::AuthKeyringBackendKind;

/// ShengSuanYun auth credentials persisted in auth storage.
#[derive(Deserialize, Serialize, Clone, PartialEq, Eq)]
pub struct ShengSuanYunAuth {
    pub api_key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jwt_token: Option<String>,
}

impl std::fmt::Debug for ShengSuanYunAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShengSuanYunAuth")
            .field("api_key", &"<redacted>")
            .field("jwt_token", &self.jwt_token.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

/// Writes auth storage containing only ShengSuanYun auth.
pub fn login_with_shengsuanyun(
    codex_home: &Path,
    api_key: &str,
    jwt_token: Option<&str>,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
    keyring_backend_kind: AuthKeyringBackendKind,
) -> std::io::Result<()> {
    let auth_dot_json = AuthDotJson {
        auth_mode: Some(AuthMode::ShengSuanYunAccessKeys),
        openai_api_key: None,
        tokens: None,
        last_refresh: None,
        agent_identity: None,
        personal_access_token: None,
        bedrock_access_keys: None,
        bedrock_api_key: None,
        shengsuanyun_access_keys: Some(ShengSuanYunAuth {
            api_key: api_key.to_string(),
            jwt_token: jwt_token.map(str::to_string),
        }),
    };
    save_auth(
        codex_home,
        &auth_dot_json,
        auth_credentials_store_mode,
        keyring_backend_kind,
    )
}
