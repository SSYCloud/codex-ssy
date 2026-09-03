# ShengSuanYun web auth — finish the implementation

## Context

A partial, uncommitted implementation of "ShengSuanYun" (胜算云) web login already exists
across the workspace (visible in `git status`/`git diff`). It wires a new `AuthMode::ShengSuanYunAccessKeys`,
a v2 RPC variant, and TUI onboarding entries, but the plumbing is incomplete and in several
places actively wrong:

- The local callback server (`login/src/server.rs`) only understands ChatGPT's OAuth/PKCE
  flow and the `/auth/callback` path. ShengSuanYun's callback is `http://localhost:1455/auth`
  (bare path) and its exchange is a simple JSON POST, not an OAuth token exchange — today
  `login_shengsuanyun_response()` starts the ChatGPT PKCE server anyway and returns a
  **hardcoded, disconnected** `auth_url` literal, so login can never actually complete.
- `login_shengsuanyun_common()` gates on `ForcedLoginMethod::Chatgpt` instead of
  `ForcedLoginMethod::ShengSuanYun`.
- `ShengSuanYunAccessKeysAuth` is shaped like Bedrock's AWS access keys
  (`access_key_id`/`secret_access_key`/`session_token`) instead of ShengSuanYun's actual
  response shape (`api_key`/`jwt_token`).
- `AuthDotJson` carries two fields of that same (wrong) type
  (`shengsuanyun_access_keys` and `shengsuanyun_api_key`) — a leftover duplicate.
- `CodexAuth` has no ShengSuanYun variant; `from_auth_dot_json()` has a lying
  `unreachable!()` arm for it, so loading persisted ShengSuanYun auth panics today.
- `ManagedAuthPolicy::allowed_login_methods()` and the model-provider bearer-auth wiring
  don't know about ShengSuanYun.
- `cli/src/doctor.rs`'s health check inspects the wrong (Bedrock-shaped) field.
- The external/wire `AuthMode::has_chatgpt_account()` in `app-server-protocol` incorrectly
  reports ShengSuanYun as a ChatGPT account, contradicting the correct internal
  `protocol/src/auth.rs` version and the user's "additive, separate feature" requirement.

Hard requirements from the user (must hold after this change):
1. **Do not break or regress ChatGPT OAuth login.**
2. ShengSuanYun auth is **strictly additive** — a new, independent login method alongside
   ChatGPT, not a replacement.
3. **No added code comments** beyond what already exists (remove misleading/stray ones
   encountered along the way, e.g. the lying `unreachable!()` message and the raw debug
   query-string comment in `account_processor.rs`).

Decisions confirmed with the user:
- Callback handling: extend the **existing** `login/src/server.rs` router (no second local
  server / no port conflict risk) rather than a standalone module.
- `ManagedAuthPolicy::allowed_login_methods()` **should** include `ForcedLoginMethod::ShengSuanYun`.
- TUI keeps ShengSuanYun highlighted/defaulted ahead of ChatGPT in onboarding (existing
  behavior in the working tree is correct — no change needed there).

## Approach

### 1. Data model: `login/src/auth/shengsuanyun_access_keys.rs` → rename to `shengsuanyun.rs`

Rename `ShengSuanYunAccessKeysAuth` → `ShengSuanYunAuth` and reshape it to match the real
response from `POST https://api.shengsuanyun.com/auth/keys`:

```rust
#[derive(Deserialize, Serialize, Clone, PartialEq, Eq)]
pub struct ShengSuanYunAuth {
    pub api_key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jwt_token: Option<String>,
}
```

Keep the redacting `Debug` impl (mirroring `BedrockApiKeyAuth`, `login/src/auth/bedrock_api_key.rs`).
Rewrite `login_with_shengsuanyun_access_keys` → `login_with_shengsuanyun(codex_home, api_key: &str,
jwt_token: Option<&str>, auth_credentials_store_mode, keyring_backend_kind)`, following the
exact `login_with_bedrock_api_key` template — construct `AuthDotJson` with
`auth_mode: Some(AuthMode::ShengSuanYunAccessKeys)` and the new field populated.

Update `login/src/auth/mod.rs` and `login/src/lib.rs` re-exports accordingly (also fix the
missing trailing newline introduced in both files).

### 2. `login/src/auth/storage.rs`: drop the duplicate field

Delete the stray `shengsuanyun_api_key: Option<ShengSuanYunAccessKeysAuth>` field entirely —
it was never read anywhere (confirmed via `resolved_mode()` and every construction site).
Keep just `shengsuanyun_access_keys: Option<ShengSuanYunAuth>` (field name unchanged, type
renamed). Update every `AuthDotJson { .. }` construction site that currently sets both fields
(`login/src/auth/manager.rs` ×5, `login/src/auth/bedrock_access_keys.rs`,
`login/src/auth/bedrock_api_key.rs`, `login/src/server.rs`,
`app-server/tests/common/auth_fixtures.rs`, `cli/src/doctor.rs` ×4) to drop the
`shengsuanyun_api_key: None,` line.

### 3. `login/src/auth/manager.rs`: add `CodexAuth::ShengSuanYun`

- Add `ShengSuanYun(ShengSuanYunAuth)` to the `CodexAuth` enum.
- `from_auth_dot_json()`: add an explicit block (alongside the `BedrockApiKey`/`BedrockAccessKeys`
  blocks around line 357-372) — `if auth_mode == AuthMode::ShengSuanYunAccessKeys { let Some(auth)
  = auth_dot_json.shengsuanyun_access_keys else { return Err(...) }; return Ok(Self::ShengSuanYun(auth)); }`.
  Replace the lying `unreachable!("胜算云 API 密钥模式在上方处理。")` arm with a correct
  `unreachable!("shengsuanyun mode is handled above")`, matching the sibling arms' English style.
- `auth_mode()` / `api_auth_mode()`: add `Self::ShengSuanYun(_) => AuthMode::ShengSuanYunAccessKeys`.
- `api_key()`: add `Self::ShengSuanYun(_)` to the `None`-returning group (same as Bedrock — this
  accessor is specifically for the plain `ApiKey` variant).
- `get_token()`: add `Self::ShengSuanYun(auth) => Ok(auth.api_key.clone())` — unlike Bedrock,
  ShengSuanYun *does* expose a Codex bearer token, since it's used directly as the model
  provider's API credential.
- `get_current_auth_json()`: add `Self::ShengSuanYun(_)` to the `None`-returning group.
- `agent_identity_auth()`: add `Self::ShengSuanYun(_)` to the `Ok(None)` group.
- `PartialEq for CodexAuth`: add `(Self::ShengSuanYun(a), Self::ShengSuanYun(b)) => a == b`.
- `auths_equal_for_refresh()`: add `(AuthMode::ShengSuanYunAccessKeys, AuthMode::ShengSuanYunAccessKeys) => a == b`.
- `AuthDotJson::resolved_mode()`: already checks `shengsuanyun_access_keys` — no change needed
  once the duplicate field is gone.
- Fix the `enforce_login_restrictions_with_agent_identity_authapi_base_url()` `method_violation`
  match (~line 1317-1351): today **every** ShengSuanYun combination resolves to `None` (no
  logout), which is inconsistent with how `Api`/`Chatgpt` already log each other out. Make it
  symmetric: forcing `ShengSuanYun` while any non-ShengSuanYun auth is active → violation
  (log out), and forcing `Api`/`Chatgpt` while `ShengSuanYunAccessKeys` is active → violation
  (log out). Only `(ShengSuanYun, ShengSuanYunAccessKeys)` stays `None`.

### 4. `login/src/server.rs`: extend the router instead of adding a second server

- Add `pub login_kind: LoginKind` to `ServerOptions` (`pub enum LoginKind { Chatgpt, ShengSuanYun }`),
  defaulting to `Chatgpt` in `ServerOptions::new()` so every existing ChatGPT call site is
  unaffected.
- In `run_login_server()`, branch on `opts.login_kind` when building `auth_url`/`redirect_uri`:
  - `Chatgpt`: unchanged existing logic.
  - `ShengSuanYun`: `callback_url = format!("http://localhost:{actual_port}/auth")`, then
    `auth_url = format!("https://router.shengsuanyun.com/auth?from=codex-ssy&callback_url={}",
    urlencoding::encode(&callback_url))`. Do **not** open-browser/PKCE-generate anything
    OAuth-specific for this branch.
- In `process_request()`, add a new `"/auth"` match arm (distinct path from `"/auth/callback"`,
  so it can never intercept ChatGPT's flow). It mirrors the `"/auth/callback"` arm's shape but:
  - extracts `code` from the query string (no `state`/PKCE — ShengSuanYun's callback has no
    state param),
  - calls a new `exchange_shengsuanyun_code(code, callback_url, auth_route_config)` that POSTs
    JSON `{"code": code, "callback_url": callback_url}` to
    `https://api.shengsuanyun.com/auth/keys?from=codex-ssy` using `create_raw_auth_client`
    (same helper `exchange_code_for_tokens` already uses), and parses
    `{code, data: {api_key, jwt_token}, msg}` — treat `code != 0` or a missing `data.api_key`
    as an error surfaced the same way `Err(err)` is handled in the sibling arm,
  - on success, persists via a new `persist_shengsuanyun_tokens_async(codex_home, api_key,
    jwt_token, auth_credentials_store_mode, keyring_backend_kind)` (mirrors `persist_tokens_async`
    but builds `AuthDotJson { auth_mode: Some(AuthMode::ShengSuanYunAccessKeys),
    shengsuanyun_access_keys: Some(ShengSuanYunAuth { api_key, jwt_token }), ..all-None }`),
  - redirects to the existing local `/success` page the same way ChatGPT does
    (`HandledRequest::RedirectWithHeader`), reusing `compose_success_url`'s local-redirect
    branch is not applicable (that function is JWT-claims-specific) — instead redirect straight
    to `http://localhost:{actual_port}/success` with no query params.

### 5. `app-server/src/request_processors/account_processor.rs`

- `login_shengsuanyun_common()`: change the gate to
  `self.auth_manager.is_login_method_allowed(ForcedLoginMethod::ShengSuanYun)`. Build
  `LoginServerOptions` with `login_kind: LoginKind::ShengSuanYun`, and stop threading
  ChatGPT-specific `oauth_client_id()` / `effective_chatgpt_workspaces()` through it (pass
  `String::new()` / `None` — they're unused for this flow). Drop the `#[cfg(debug_assertions)]`
  issuer-override block (ChatGPT-only, meaningless here).
- `login_shengsuanyun_response()`: remove the hardcoded `auth_url` literal and the stray debug
  comment (lines ~863-864). Capture `let auth_url = server.auth_url.clone();` **before** moving
  `server` into the spawned task (exactly like `login_chatgpt_response()` does), and return
  `LoginAccountResponse::ShengSuanYun { login_id: login_id.to_string(), auth_url }`.

### 6. Peripheral wiring

- `model-provider/src/auth.rs`: add `CodexAuth::ShengSuanYun(_)` to the `BearerAuthProvider`
  group in `auth_provider_from_auth()` (not the Bedrock `unreachable!()` group) — this is what
  makes ShengSuanYun's `api_key` actually get used as the outbound bearer credential.
- `config/src/auth_policy.rs`: add `ForcedLoginMethod::ShengSuanYun` to the array in
  `allowed_login_methods()` (confirmed with user).
- `cli/src/doctor.rs`: fix `stored_auth_issues()`'s `AuthMode::ShengSuanYunAccessKeys` arm to
  check `tokens.api_key.trim().is_empty()` instead of the nonexistent-post-rename
  `session_token` field, following the same push-per-missing-field pattern as the
  `BedrockAccessKeys` arm just above it.
- `app-server-protocol/src/protocol/common.rs`: fix `has_chatgpt_account()` — move
  `Self::ShengSuanYunAccessKeys` out of the `true` branch and into the `false` branch (with
  `ApiKey`/`Headers`/`AgentIdentity`/`BedrockApiKey`/`BedrockAccessKeys`), matching the already-correct
  `protocol/src/auth.rs` semantics.

### 7. Cleanup pass

Several already-present diffs have formatting slips worth fixing while touching these files
(not new comments — just correctness/style): missing space before `=>` in several
`... | Self::ShengSuanYunAccessKeys=> ...` match arms (`protocol/src/auth.rs`,
`app-server-protocol/src/protocol/common.rs`, `core/src/client.rs`, `otel/src/lib.rs`,
`cli/src/doctor.rs`), missing trailing newline in `login/src/auth/mod.rs` and `login/src/lib.rs`,
and the leading `|` on the first match arm in
`tui/src/app_server_session.rs::status_account_display_from_auth_mode`. Run `cargo fmt` across
touched crates at the end.

## Files touched (representative, not exhaustive for mechanical field-drop edits)

- `login/src/auth/shengsuanyun_access_keys.rs` → renamed `login/src/auth/shengsuanyun.rs`
- `login/src/auth/mod.rs`, `login/src/lib.rs`
- `login/src/auth/storage.rs`
- `login/src/auth/manager.rs`
- `login/src/server.rs`
- `app-server/src/request_processors/account_processor.rs`
- `model-provider/src/auth.rs`
- `config/src/auth_policy.rs`
- `cli/src/doctor.rs`
- `app-server-protocol/src/protocol/common.rs`
- Mechanical `shengsuanyun_api_key: None,` removal: `login/src/auth/bedrock_access_keys.rs`,
  `login/src/auth/bedrock_api_key.rs`, `app-server/tests/common/auth_fixtures.rs`

No changes needed (already correct in the working tree): `app-server-protocol/src/protocol/v2/account.rs`
(the `ShengSuanYun` variants' camelCase auto-derivation is correct without explicit rename
attributes), `app-server/src/auth_mode.rs`, `tui/src/onboarding/auth.rs` /
`tui/src/onboarding/onboarding_screen.rs` (ordering already matches the confirmed decision),
`tui/src/status/account.rs`, `tui/src/status/card.rs`, `protocol/src/config_types.rs`,
`protocol/src/auth.rs`.

## Verification

1. `cargo build -p codex-login -p codex-app-server -p codex-app-server-protocol -p codex-cli
   -p codex-tui -p codex-model-provider -p codex-config -p codex-protocol -p codex-core -p codex-otel`
   — confirm the workspace compiles with no `unreachable!()` panics reachable and no leftover
   references to the old `ShengSuanYunAccessKeysAuth` name.
2. `cargo test -p codex-login -p codex-app-server -p codex-cli` — existing ChatGPT/Bedrock auth
   tests must still pass unmodified (regression guard for requirement #1).
3. Manual end-to-end: run the TUI (`cargo run -p codex-tui`), select ShengSuanYun sign-in,
   confirm the opened browser URL is `https://router.shengsuanyun.com/auth?from=codex-ssy&callback_url=http%3A%2F%2Flocalhost%3A1455%2Fauth`,
   then hit `http://localhost:1455/auth?code=<test-code>` manually (or via the real flow) and
   confirm: the local server responds, `auth.json` gets a `shengsuanyun_access_keys` entry with
   `api_key`/`jwt_token`, and `codex login status` / the TUI status card show the ShengSuanYun
   account.
4. Re-run a ChatGPT login end-to-end (`codex login`) to confirm requirement #1 — the existing
   OAuth/PKCE flow through `/auth/callback` is completely unaffected by the new `/auth` arm and
   `LoginKind` field.
