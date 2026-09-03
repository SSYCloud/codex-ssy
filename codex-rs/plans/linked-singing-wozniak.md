# ShengSuanYun model listing + invocation

## Context

ShengSuanYun (胜算云) is a Chinese LLM router that speaks an OpenAI-compatible
Responses API. Login/auth storage for it already exists and is being built
separately (`AuthMode::ShengSuanYunAccessKeys`, `CodexAuth::ShengSuanYun`,
`ShengSuanYunAuth { api_key, jwt_token }` in `login/src/auth/shengsuanyun.rs`).
What's still missing is making Codex actually *use* that auth to (1) fetch
ShengSuanYun's live model catalog from `GET /api/v1/models` (no auth
required) and filter it to Responses-API-capable models, and (2) route
inference requests to ShengSuanYun's `/api/v1/responses` endpoint with the
right base URL and model slug.

`ConfiguredModelProvider::account_state()` already treats
`CodexAuth::ShengSuanYun(_)` as `ProviderAccount::ApiKey`, confirming
ShengSuanYun is meant to ride the existing generic (non-Bedrock)
`ConfiguredModelProvider` path — no new `model_provider` config entry, no
dedicated provider struct. Bearer-token auth wiring
(`auth_provider_from_auth` in `model-provider/src/auth.rs`) already groups
`ShengSuanYun` with `ApiKey`/etc., so invocation auth is a non-issue once the
base URL resolves correctly. The only real gaps are: base URL resolution,
and a `ModelsEndpointClient` implementation that understands ShengSuanYun's
non-Codex model-list schema.

Verified against the live service via curl: `GET
https://router.shengsuanyun.com/api/v1/models` returns models with an
`support_apis` array; only entries containing `"/v1/responses"` are usable.
`POST https://router.shengsuanyun.com/api/v1/responses` is a real,
correctly-routed endpoint (401 invalid_api_key without a key, not
404/405). So `https://router.shengsuanyun.com/api/v1` is the correct
base_url, parallel to OpenAI's `https://api.openai.com/v1` — once set,
`Provider::url_for_path()` (`codex-api/src/provider.rs`) naturally produces
correct URLs for both `/models` and `/responses` with no endpoint-specific
URL logic.

## Changes

### 1. Base URL resolution — `model-provider-info/src/lib.rs`

In `ModelProviderInfo::to_api_provider()` (~line 292), add a
`ShengSuanYunAccessKeys` branch to the `default_base_url` calculation
(currently branches only on ChatGPT-family auth vs. the OpenAI default):

```rust
} else if matches!(auth_mode, Some(AuthMode::ShengSuanYunAccessKeys)) {
    SHENGSUANYUN_BASE_URL
} else {
    "https://api.openai.com/v1"
};
```

Add `pub const SHENGSUANYUN_BASE_URL: &str = "https://router.shengsuanyun.com/api/v1";`
next to the existing `CHATGPT_CODEX_BASE_URL` const, and re-export it from
`model-provider/src/lib.rs` (mirrors the existing `pub use
codex_model_provider_info::CHATGPT_CODEX_BASE_URL;` line) so the new
endpoint module can reference it without duplicating the string. This only
takes effect when the provider's own `base_url` is unset, which is the case
for the default `"OpenAI"`-named provider entry that ShengSuanYun auth rides.

### 2. New module — `model-provider/src/shengsuanyun_models_endpoint.rs`

Model this directly on `OpenAiModelsEndpoint`
(`model-provider/src/models_endpoint.rs`), reusing the exact same
transport-building pattern (`create_client_for_route_async` +
`ReqwestTransport`, `resolve_provider_auth`, `enforce_managed_residency`, a
5s `timeout()`), but **do not reuse `ModelsClient`** — it hardcodes
deserialization into Codex's own `ModelsResponse{models: ...}` shape, which
won't match ShengSuanYun's `{"data": [...], "object": "list", "success":
true}` shape. Instead build the request by hand via
`api_provider.build_request(Method::GET, "models")`, apply auth via
`api_auth.apply_auth(request).await`, run it through `ReqwestTransport`
directly (implements `HttpTransport`), and `serde_json::from_slice` the body
into a local response struct. Skip request telemetry/query-param plumbing
that `OpenAiModelsEndpoint` does for Codex's endpoint — not needed here.

Raw schema (use `#[serde(default)]` liberally since this is an
external/unversioned API — fields the plan doesn't need, like `pricing`,
`company`, `architecture`, `supports_prompt_cache`, `max_tokens`, are simply
not modeled and dropped):

```rust
#[derive(Debug, Deserialize)]
struct ShengSuanYunModelsResponse {
    data: Vec<ShengSuanYunModel>,
}

#[derive(Debug, Deserialize)]
struct ShengSuanYunModel {
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    context_window: Option<i64>,
    #[serde(default)]
    support_apis: Vec<String>,
}
```

Fetch → filter by `support_apis.contains("/v1/responses")` → map each
surviving entry to `ModelInfo`
(`protocol/src/openai_models.rs`). Field mapping:

- `slug = id`, `display_name = name` (fall back to `id` if `name` empty) —
  `id`/`api_name` are identical in the sample data; `id` is canonical.
- `context_window = max_context_window = context_window` (ShengSuanYun has
  one field, no separate max).
- `description`: `Some(..)` if non-empty, else `None`.
- `priority = 100` (flat constant) — below the bundled catalog's 1–43 range,
  since there's no real ranking signal from the API and relative order
  among ~190 models is otherwise arbitrary.
- `visibility = ModelVisibility::List`, `supported_in_api = true` — both
  required: `ModelPreset::filter_by_auth` drops non-`supported_in_api`
  models whenever auth isn't ChatGPT-mode (always true for ShengSuanYun),
  and `apply_remote_models()`'s replace-vs-merge check (change 4 below)
  requires at least one `List`-visibility model.
- Everything else Codex-specific and unavailable from ShengSuanYun
  (`shell_type`, `truncation_policy`, `effective_context_window_percent`,
  `input_modalities`, `web_search_tool_type`, `apply_patch_tool_type`,
  `model_messages.instructions_template`, `supported_reasoning_levels`,
  etc.) — default exactly per `models_manager::model_info::model_info_from_slug()`
  (`models-manager/src/model_info.rs:140`), which is precisely this
  "minimally valid fallback `ModelInfo`" pattern. Use
  `codex_models_manager::model_info::BASE_INSTRUCTIONS` for
  `model_messages.instructions_template` (already `pub`, confirmed).

`ModelsEndpointClient` impl (trait in `models-manager/src/manager.rs`):

```rust
impl ModelsEndpointClient for ShengSuanYunModelsEndpoint {
    fn has_command_auth(&self) -> bool {
        self.provider_info.has_command_auth()
    }

    fn uses_codex_backend(&self) -> ModelsEndpointFuture<'_, bool> {
        // Always force-refresh: ShengSuanYun's catalog changes frequently and
        // there's no other trigger for it (see should_refresh_models() below).
        // This does NOT claim ShengSuanYun auth is Codex-backed —
        // AuthMode::uses_codex_backend() correctly stays false for it elsewhere
        // (e.g. ModelPreset::filter_by_auth's chatgpt_mode gate).
        Box::pin(async { true })
    }

    fn list_models<'a>(&'a self, client_version: &'a str, http_client_factory: HttpClientFactory)
        -> ModelsEndpointFuture<'a, CoreResult<(Vec<ModelInfo>, Option<String>)>> {
        Box::pin(Self::list_models(self, client_version, http_client_factory))
    }
}
```

**Why `uses_codex_backend()` must return `true`:** `ModelsManager`'s
`should_refresh_models()` (`models-manager/src/manager.rs`) is `endpoint_client.uses_codex_backend()
|| endpoint_client.has_command_auth()`, and gates every network refresh
attempt. Plain API-key OpenAI auth returns `false` for both, so it never
force-refreshes — acceptable since OpenAI's model list is effectively
static and covered by the bundled catalog. ShengSuanYun's whole point is a
large, dynamic multi-provider catalog, so it must always attempt a live
refresh; returning `true` here is the simplest correct way to opt into that
existing refresh-gating mechanism without touching `should_refresh_models()`
itself.

Register the module in `model-provider/src/lib.rs`: add `mod
shengsuanyun_models_endpoint;` next to `mod models_endpoint;` (private, same
as its sibling — reached via `crate::` path from `provider.rs`, no `pub use`
needed).

### 3. Wiring — `model-provider/src/provider.rs`

`ConfiguredModelProvider::models_manager()` /
`models_manager_without_cache()` / `models_manager_with_cache()` (~lines
434–502) all unconditionally build `OpenAiModelsEndpoint::new(...)` in their
`config_model_catalog.is_none()` branch. Add a small helper and branch in
all three:

```rust
fn uses_shengsuanyun_auth(&self) -> bool {
    self.auth_manager
        .as_ref()
        .and_then(|m| m.auth_cached())
        .map(|a| a.auth_mode())
        == Some(AuthMode::ShengSuanYunAccessKeys)
}
```

```rust
let endpoint: Arc<dyn ModelsEndpointClient> = if self.uses_shengsuanyun_auth() {
    Arc::new(ShengSuanYunModelsEndpoint::new(self.info.clone(), self.auth_manager.clone()))
} else {
    Arc::new(OpenAiModelsEndpoint::new(self.info.clone(), self.auth_manager.clone()))
};
```

(then feed `endpoint` into whichever `OpenAiModelsManager::new*` constructor
that method already calls). Same pattern, three call sites, differing only
in which manager constructor wraps the result.

### 4. Merge-vs-replace — `models-manager/src/manager.rs`

`apply_remote_models()` currently makes remote models fully replace the
in-memory catalog only if non-empty, contains ≥1 `ModelVisibility::List`
model, and `auth_mode.has_chatgpt_account()`. A ShengSuanYun user has no
OpenAI/ChatGPT credentials and can't invoke the bundled catalog's models at
all, so merging them in would just add uninvokable noise to the picker.
Extend the auth predicate:

```rust
auth_manager.auth_mode().is_some_and(|mode| {
    mode.has_chatgpt_account() || mode.has_shengsuanyun_account()
})
```

`AuthMode::has_shengsuanyun_account()` already exists
(`protocol/src/auth.rs:73`) purpose-built for this. One-line boolean change,
no signature changes.

## Verification

- `cargo test -p codex-model-provider-info` — add a case to the existing
  `to_api_provider` test module asserting `ShengSuanYunAccessKeys` with no
  explicit `base_url` resolves to `SHENGSUANYUN_BASE_URL`, and that an
  explicit override still wins.
- `cargo test -p codex-model-provider` — add a wiremock-based test module in
  `shengsuanyun_models_endpoint.rs` (mirroring `models_endpoint.rs`'s own
  tests): mount `GET /models` returning a 2-entry fixture (one with
  `/v1/responses` in `support_apis`, one without), assert exactly 1 survives
  and its mapped fields (`slug`, `display_name`, `context_window`,
  `visibility`, `supported_in_api`, non-empty
  `model_messages.instructions_template`) are correct. Include the
  real sample JSON from ShengSuanYun's docs as the fixture.
- `cargo test -p codex-models-manager` — extend `manager_tests.rs` with a
  ShengSuanYun-auth case asserting `apply_remote_models` replaces (not
  merges) the bundled catalog, plus a regression case confirming plain
  API-key auth still merges.
- `cargo test -p codex-model-provider` — extend `provider.rs`'s existing
  `models_manager(...)` tests with a ShengSuanYun-authed
  `ConfiguredModelProvider`, asserting no panic/error and (via existing
  `account_state()` assertions) that behavior outside model listing is
  unchanged.
- Manual smoke check: with a real ShengSuanYun API key configured, run the
  CLI/TUI, open the model picker, and confirm ShengSuanYun models appear and
  a chat turn against one of them succeeds end-to-end.
