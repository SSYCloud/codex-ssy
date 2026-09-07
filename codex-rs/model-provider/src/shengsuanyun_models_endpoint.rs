use std::sync::Arc;
use std::time::Duration;

use codex_api::ReqwestTransport;
use codex_api::TransportError;
use codex_client::HttpTransport;
use codex_http_client::ClientRouteClass;
use codex_http_client::HttpClientFactory;
use codex_login::AuthManager;
use codex_login::CodexAuth;
use codex_login::default_client::create_client_for_route_async;
use codex_model_provider_info::ModelProviderInfo;
use codex_models_manager::manager::ModelsEndpointClient;
use codex_models_manager::manager::ModelsEndpointFuture;
use codex_models_manager::model_info::BASE_INSTRUCTIONS;
use codex_protocol::config_types::ReasoningSummary;
use codex_protocol::error::CodexErr;
use codex_protocol::error::Result as CoreResult;
use codex_protocol::openai_models::ConfigShellToolType;
use codex_protocol::openai_models::ModelInfo;
use codex_protocol::openai_models::ModelMessages;
use codex_protocol::openai_models::ModelVisibility;
use codex_protocol::openai_models::TruncationPolicyConfig;
use codex_protocol::openai_models::WebSearchToolType;
use codex_protocol::openai_models::default_input_modalities;
use http::Method;
use serde::Deserialize;
use tokio::time::timeout;

use crate::auth::resolve_provider_auth;
use crate::provider::enforce_managed_residency;

const MODELS_REFRESH_TIMEOUT: Duration = Duration::from_secs(5);
const RESPONSES_API_SUPPORT: &str = "/v1/responses";
/// Below the bundled catalog's priority range (1..=43): ShengSuanYun's ~190
/// models have no ranking signal from the API, so they're all deprioritized
/// equally relative to the curated bundled models.
const SHENGSUANYUN_FALLBACK_PRIORITY: i32 = 100;

/// ShengSuanYun's provider-owned `/models` endpoint.
///
/// Unlike `OpenAiModelsEndpoint`, this cannot reuse `codex_api::ModelsClient`
/// because ShengSuanYun's `/models` response has its own schema
/// (`{"data": [...], "object": "list", "success": true}`), not Codex's
/// `ModelsResponse` shape.
#[derive(Debug)]
pub(crate) struct ShengSuanYunModelsEndpoint {
    provider_info: ModelProviderInfo,
    auth_manager: Option<Arc<AuthManager>>,
}

impl ShengSuanYunModelsEndpoint {
    pub(crate) fn new(
        provider_info: ModelProviderInfo,
        auth_manager: Option<Arc<AuthManager>>,
    ) -> Self {
        Self {
            provider_info,
            auth_manager,
        }
    }

    async fn auth(&self) -> Option<CodexAuth> {
        match self.auth_manager.as_ref() {
            Some(auth_manager) => auth_manager.auth().await,
            None => None,
        }
    }

    async fn list_models(
        &self,
        _client_version: &str,
        http_client_factory: HttpClientFactory,
    ) -> CoreResult<(Vec<ModelInfo>, Option<String>)> {
        let auth = self.auth().await;
        let auth_mode = auth.as_ref().map(CodexAuth::auth_mode);
        let mut api_provider = self.provider_info.to_api_provider(auth_mode)?;
        enforce_managed_residency(&mut api_provider);
        let api_auth = resolve_provider_auth(auth.as_ref(), &self.provider_info)?;

        let request = api_provider.build_request(Method::GET, "models");
        let request = api_auth
            .apply_auth(request)
            .await
            .map_err(|err| CodexErr::Stream(err.to_string()))?;

        timeout(MODELS_REFRESH_TIMEOUT, async {
            let http_client = create_client_for_route_async(
                http_client_factory,
                request.url.clone(),
                ClientRouteClass::Api,
            )
            .await
            .map_err(|err| CodexErr::Stream(err.to_string()))?;
            let transport = ReqwestTransport::from_http_client(http_client);
            let response = transport
                .execute(request)
                .await
                .map_err(map_transport_error)?;
            let parsed: ShengSuanYunModelsResponse =
                serde_json::from_slice(&response.body).map_err(|err| {
                    CodexErr::Stream(format!(
                        "failed to decode shengsuanyun models response: {err}; body: {}",
                        String::from_utf8_lossy(&response.body)
                    ))
                })?;
            let models: Vec<ModelInfo> = parsed
                .data
                .into_iter()
                .filter(|model| {
                    model
                        .support_apis
                        .as_deref()
                        .unwrap_or_default()
                        .iter()
                        .any(|api| api == RESPONSES_API_SUPPORT)
                })
                .map(shengsuanyun_model_to_model_info)
                .collect();
            Ok((models, /* etag */ None))
        })
        .await
        .map_err(|_| CodexErr::Timeout)?
    }
}

fn map_transport_error(error: TransportError) -> CodexErr {
    CodexErr::Stream(error.to_string())
}

impl ModelsEndpointClient for ShengSuanYunModelsEndpoint {
    fn has_command_auth(&self) -> bool {
        self.provider_info.has_command_auth()
    }

    fn uses_codex_backend(&self) -> ModelsEndpointFuture<'_, bool> {
        // Always force a live refresh
        Box::pin(async { true })
    }

    fn list_models<'a>(
        &'a self,
        client_version: &'a str,
        http_client_factory: HttpClientFactory,
    ) -> ModelsEndpointFuture<'a, CoreResult<(Vec<ModelInfo>, Option<String>)>> {
        Box::pin(Self::list_models(self, client_version, http_client_factory))
    }
}

#[derive(Debug, Deserialize)]
struct ShengSuanYunModelsResponse {
    #[serde(default)]
    data: Vec<ShengSuanYunModel>,
}

#[derive(Debug, Deserialize)]
struct ShengSuanYunModel {
    id: String,
    #[serde(default)]
    name: String,
    // #[serde(default)]
    // description: String,
    #[serde(default)]
    context_window: Option<i64>,
    #[serde(default)]
    pricing: Option<Pricing>,
    #[serde(default)]
    support_apis: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
struct Pricing {
    input_price: Option<f32>,
    #[serde(default)]
    output_price: Option<f32>,
    // #[serde(default)]
    // cached_price: Option<f32>,
    // #[serde(default)]
    // image_price: Option<f32>,
    #[serde(default)]
    currency: String,
}
fn format_description(model: &ShengSuanYunModel) -> String {
    let context_window = model
        .context_window
        .map(|v| (v / 1000).to_string())
        .unwrap_or_default();

    let (input_price, output_price, currency) = match model.pricing.as_ref() {
        Some(pricing) => (
            pricing
                .input_price
                .map(|v| v.to_string())
                .unwrap_or_default(),
            pricing
                .output_price
                .map(|v| v.to_string())
                .unwrap_or_default(),
            pricing.currency.clone(),
        ),
        None => (String::new(), String::new(), String::new()),
    };
    format!(
        "上下文：{context_window:>5} K  输入价格：{input_price:>5} {currency}/M 输出价格：{output_price:>5} {currency}/M"
    )
}

fn shengsuanyun_model_to_model_info(model: ShengSuanYunModel) -> ModelInfo {
    let description = format_description(&model);
    let display_name = if model.name.is_empty() {
        model.id.clone()
    } else {
        model.name
    };
    ModelInfo {
        guardian: None,
        slug: model.id,
        display_name,
        description: Some(description),
        default_reasoning_level: None,
        supported_reasoning_levels: Vec::new(),
        shell_type: ConfigShellToolType::UnifiedExec,
        visibility: ModelVisibility::List,
        supported_in_api: true,
        priority: SHENGSUANYUN_FALLBACK_PRIORITY,
        additional_speed_tiers: Vec::new(),
        service_tiers: Vec::new(),
        default_service_tier: None,
        availability_nux: None,
        upgrade: None,
        model_messages: Some(ModelMessages {
            persistent_instructions: None,
            tools: None,
            confirmation_policies: None,
            instructions_template: Some(BASE_INSTRUCTIONS.to_string()),
            instructions_variables: None,
            approvals: None,
            collaboration_modes: None,
            auto_review: None,
            permissions: None,
            multi_agent: None,
            token_budget: None,
            guardian_v2: None,
        }),
        include_skills_usage_instructions: false,
        include_plugin_usage_instructions: false,
        include_apps_usage_instructions: false,
        supports_reasoning_summary_parameter: true,
        default_reasoning_summary: ReasoningSummary::Auto,
        support_verbosity: false,
        default_verbosity: None,
        apply_patch_tool_type: None,
        web_search_tool_type: WebSearchToolType::Text,
        truncation_policy: TruncationPolicyConfig::bytes(/* limit */ 10_000),
        supports_image_detail_original: false,
        context_window: model.context_window,
        max_context_window: model.context_window,
        auto_compact_token_limit: None,
        comp_hash: None,
        effective_context_window_percent: 95,
        experimental_supported_tools: Vec::new(),
        input_modalities: default_input_modalities(),
        used_fallback_model_metadata: false,
        supports_search_tool: false,
        supports_experimental_context: false,
        use_responses_lite: false,
        node_repl_auto_review_required: false,
        node_repl_disabled: false,
        auto_review_model_override: None,
        model_specialty: None,
        tool_mode: None,
        multi_agent_version: None,
        multi_agent_reasoning_effort: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_http_client::OutboundProxyPolicy;
    use pretty_assertions::assert_eq;
    use wiremock::Mock;
    use wiremock::MockServer;
    use wiremock::ResponseTemplate;
    use wiremock::matchers::method;
    use wiremock::matchers::path;

    fn sample_body() -> serde_json::Value {
        serde_json::json!({
            "data": [
                {
                    "company": "DeepSeek",
                    "name": "DeepSeek-V4-Pro-0813",
                    "api_name": "deepseek/deepseek-v4-pro",
                    "description": "A great model",
                    "max_tokens": 384000,
                    "context_window": 1000000,
                    "supports_prompt_cache": true,
                    "architecture": {"input": "text", "output": "text", "tokenizer": ""},
                    "pricing": {
                        "price": 0, "input_price": 4.5, "output_price": 13.5,
                        "cached_price": 0.15, "image_price": 0, "currency": "CNY"
                    },
                    "id": "deepseek/deepseek-v4-pro",
                    "support_apis": [
                        "/v1/chat/completions", "/v1/messages", "/v1/responses",
                        "/v1beta/models/*", "/v1/models/*"
                    ]
                }
            ],
            "object": "list",
            "success": true
        })
    }

    #[tokio::test]
    async fn list_models_filters_to_responses_api_and_maps_fields() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/models"))
            .respond_with(ResponseTemplate::new(200).set_body_json(sample_body()))
            .expect(1)
            .mount(&server)
            .await;

        let endpoint = ShengSuanYunModelsEndpoint::new(
            ModelProviderInfo::create_openai_provider(Some(server.uri())),
            /* auth_manager */ None,
        );

        let (models, _etag) = endpoint
            .list_models(
                "0.0.0",
                HttpClientFactory::new(OutboundProxyPolicy::RespectSystemProxy),
            )
            .await
            .expect("models request should succeed");

        assert_eq!(models.len(), 1);
        let model = &models[0];
        assert_eq!(model.slug, "deepseek/deepseek-v4-pro");
        assert_eq!(model.display_name, "DeepSeek-V4-Pro-0813");
        assert_eq!(model.context_window, Some(1_000_000));
        assert_eq!(model.max_context_window, Some(1_000_000));
        assert_eq!(model.visibility, ModelVisibility::List);
        assert!(model.supported_in_api);
        assert_eq!(
            model
                .model_messages
                .as_ref()
                .and_then(|messages| messages.instructions_template.as_deref()),
            Some(BASE_INSTRUCTIONS)
        );
    }

    #[tokio::test]
    async fn uses_codex_backend_is_always_true() {
        let endpoint = ShengSuanYunModelsEndpoint::new(
            ModelProviderInfo::create_openai_provider(/* base_url */ None),
            /* auth_manager */ None,
        );

        assert!(
            ModelsEndpointClient::uses_codex_backend(&endpoint).await,
            "shengsuanyun endpoint must always force a refresh"
        );
    }
}
