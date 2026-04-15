use chrono::{DateTime, Utc};
use protocol::{
    JudgeBundle, LlmDragonEvaluation, LlmJudgeEvaluation, LlmProviderEntry, LlmProviderKind,
    SpriteSet,
};
use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
use serde::{Deserialize, Serialize};
use std::{
    env, fs,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::sync::Mutex;
use tracing::{info, warn};

// ---------------------------------------------------------------------------
// Provider pool
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub(crate) struct ResolvedProvider {
    pub(crate) kind: LlmProviderKind,
    pub(crate) model: String,
    pub(crate) api_key: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct LlmPoolConfig {
    pub(crate) google_cloud_project: Option<String>,
    pub(crate) google_cloud_location: Option<String>,
    pub(crate) judge_providers: Vec<ResolvedProvider>,
    pub(crate) image_providers: Vec<ResolvedProvider>,
}

impl LlmPoolConfig {
    pub(crate) fn is_judge_configured(&self) -> bool {
        !self.judge_providers.is_empty()
    }

    pub(crate) fn is_image_configured(&self) -> bool {
        !self.image_providers.is_empty()
    }
}

/// Parse provider entries from a JSON env var and resolve API keys.
pub(crate) fn resolve_providers(entries: &[LlmProviderEntry], role: &str) -> Vec<ResolvedProvider> {
    entries
        .iter()
        .enumerate()
        .filter_map(|(i, entry)| {
            let api_key = match entry.provider_type {
                LlmProviderKind::ApiKey => {
                    let env_name = entry
                        .api_key_env_var
                        .clone()
                        .unwrap_or_else(|| format!("LLM_{}_API_KEY_{}", role.to_uppercase(), i));
                    match env::var(&env_name) {
                        Ok(key) if !key.trim().is_empty() => Some(key.trim().to_string()),
                        _ => {
                            warn!(
                                provider_index = i,
                                role,
                                env_name,
                                "api_key provider missing API key env var, skipping"
                            );
                            return None;
                        }
                    }
                }
                LlmProviderKind::VertexAi => None,
            };
            Some(ResolvedProvider {
                kind: entry.provider_type.clone(),
                model: entry.model.clone(),
                api_key,
            })
        })
        .collect()
}

pub(crate) fn load_llm_pool_config() -> Result<LlmPoolConfig, String> {
    let google_cloud_project = env::var("GOOGLE_CLOUD_PROJECT")
        .ok()
        .filter(|v| !v.trim().is_empty());
    let google_cloud_location = env::var("GOOGLE_CLOUD_LOCATION")
        .ok()
        .filter(|v| !v.trim().is_empty());

    let judge_entries: Vec<LlmProviderEntry> = match env::var("LLM_JUDGE_PROVIDERS") {
        Ok(json) if !json.trim().is_empty() => serde_json::from_str(&json)
            .map_err(|e| format!("invalid LLM_JUDGE_PROVIDERS JSON: {e}"))?,
        _ => Vec::new(),
    };

    let image_entries: Vec<LlmProviderEntry> = match env::var("LLM_IMAGE_PROVIDERS") {
        Ok(json) if !json.trim().is_empty() => serde_json::from_str(&json)
            .map_err(|e| format!("invalid LLM_IMAGE_PROVIDERS JSON: {e}"))?,
        _ => Vec::new(),
    };

    let judge_providers = resolve_providers(&judge_entries, "judge");
    let image_providers = resolve_providers(&image_entries, "image");

    let uses_vertex_ai = judge_providers
        .iter()
        .chain(image_providers.iter())
        .any(|provider| provider.kind == LlmProviderKind::VertexAi);

    if uses_vertex_ai && google_cloud_project.is_none() {
        return Err(
            "GOOGLE_CLOUD_PROJECT is required when any LLM provider uses vertex_ai".to_string(),
        );
    }

    if uses_vertex_ai && google_cloud_location.is_none() {
        return Err(
            "GOOGLE_CLOUD_LOCATION is required when any LLM provider uses vertex_ai".to_string(),
        );
    }

    info!(
        judge_provider_count = judge_providers.len(),
        image_provider_count = image_providers.len(),
        "LLM provider pool initialized"
    );

    Ok(LlmPoolConfig {
        google_cloud_project,
        google_cloud_location,
        judge_providers,
        image_providers,
    })
}

// ---------------------------------------------------------------------------
// GCE metadata token (for Vertex AI / Workload Identity)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct AccessTokenResponse {
    access_token: String,
    #[allow(dead_code)]
    expires_in: u64,
    #[allow(dead_code)]
    token_type: String,
}

#[derive(Clone)]
pub(crate) struct GceTokenCache {
    token: Arc<Mutex<Option<CachedToken>>>,
    client: reqwest::Client,
}

#[derive(Debug, Clone, Deserialize)]
struct ServiceAccountCredentials {
    client_email: String,
    private_key: String,
    #[serde(default = "default_google_token_uri")]
    token_uri: String,
}

#[derive(Debug, Clone, Deserialize)]
struct AuthorizedUserCredentials {
    client_id: String,
    client_secret: String,
    refresh_token: String,
    #[serde(default = "default_google_token_uri")]
    token_uri: String,
    #[serde(default)]
    service_account_impersonation_url: Option<String>,
}

#[derive(Debug, Clone)]
enum GoogleApplicationCredentials {
    ServiceAccount(ServiceAccountCredentials),
    AuthorizedUser(AuthorizedUserCredentials),
}

#[derive(Serialize)]
struct ServiceAccountJwtClaims<'a> {
    iss: &'a str,
    scope: &'a str,
    aud: &'a str,
    exp: usize,
    iat: usize,
}

#[derive(Serialize)]
struct ServiceAccountTokenRequest<'a> {
    grant_type: &'a str,
    assertion: &'a str,
}

#[derive(Serialize)]
struct AuthorizedUserTokenRequest<'a> {
    client_id: &'a str,
    client_secret: &'a str,
    refresh_token: &'a str,
    grant_type: &'a str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ImpersonatedTokenRequest<'a> {
    scope: &'a [&'a str],
    lifetime: &'a str,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ImpersonatedTokenResponse {
    access_token: String,
    expire_time: String,
}

#[derive(Clone)]
struct CachedToken {
    access_token: String,
    expires_at: std::time::Instant,
}

fn default_google_token_uri() -> String {
    "https://oauth2.googleapis.com/token".to_string()
}

fn build_cached_token(access_token: String, expires_in: u64) -> CachedToken {
    CachedToken {
        access_token,
        expires_at: std::time::Instant::now() + Duration::from_secs(expires_in),
    }
}

fn build_cached_token_from_expire_time(
    access_token: String,
    expire_time: &str,
) -> Result<CachedToken, LlmError> {
    let expires_at = DateTime::parse_from_rfc3339(expire_time).map_err(|e| {
        LlmError::ProviderUnavailable(format!("parse impersonated token expiry: {e}"))
    })?;
    let remaining_seconds = (expires_at.with_timezone(&Utc) - Utc::now())
        .num_seconds()
        .max(0) as u64;
    Ok(build_cached_token(access_token, remaining_seconds))
}

fn configured_impersonation_url(adc_url: Option<&str>) -> Option<String> {
    env::var("GOOGLE_IMPERSONATE_SERVICE_ACCOUNT")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .map(|service_account| {
            format!(
                "https://iamcredentials.googleapis.com/v1/projects/-/serviceAccounts/{service_account}:generateAccessToken"
            )
        })
        .or_else(|| {
            adc_url
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
        })
}

impl GceTokenCache {
    pub(crate) fn new(client: reqwest::Client) -> Self {
        Self {
            token: Arc::new(Mutex::new(None)),
            client,
        }
    }

    pub(crate) async fn get_token(&self) -> Result<String, LlmError> {
        {
            let guard = self.token.lock().await;
            if let Some(cached) = guard.as_ref() {
                if cached.expires_at > std::time::Instant::now() + Duration::from_secs(30) {
                    return Ok(cached.access_token.clone());
                }
            }
        }

        if let Some(token) = env::var("GOOGLE_OAUTH_ACCESS_TOKEN")
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
        {
            let expires_in = env::var("GOOGLE_OAUTH_ACCESS_TOKEN_EXPIRES_IN")
                .ok()
                .and_then(|value| value.trim().parse::<u64>().ok())
                .filter(|value| *value > 60)
                .unwrap_or(3000);
            let cached = build_cached_token(token.clone(), expires_in);
            *self.token.lock().await = Some(cached);
            return Ok(token);
        }

        if let Some(path) = env::var("GOOGLE_APPLICATION_CREDENTIALS")
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
        {
            let cached = fetch_google_application_credentials_token(&self.client, &path).await?;
            let access_token = cached.access_token.clone();
            *self.token.lock().await = Some(cached);
            return Ok(access_token);
        }

        let response = self
            .client
            .get("http://metadata.google.internal/computeMetadata/v1/instance/service-accounts/default/token")
            .header("Metadata-Flavor", "Google")
            .timeout(Duration::from_secs(5))
            .send()
            .await
            .map_err(|e| LlmError::ProviderUnavailable(format!("metadata server: {e}")))?;

        if !response.status().is_success() {
            return Err(LlmError::ProviderUnavailable(format!(
                "metadata server returned {}",
                response.status()
            )));
        }

        let body: AccessTokenResponse = response
            .json()
            .await
            .map_err(|e| LlmError::ProviderUnavailable(format!("metadata token parse: {e}")))?;

        let cached = build_cached_token(body.access_token.clone(), body.expires_in);
        *self.token.lock().await = Some(cached);

        Ok(body.access_token)
    }
}

fn load_google_application_credentials(path: &str) -> Result<GoogleApplicationCredentials, LlmError> {
    let raw = fs::read_to_string(path).map_err(|e| {
        LlmError::ProviderUnavailable(format!(
            "read GOOGLE_APPLICATION_CREDENTIALS file {path}: {e}"
        ))
    })?;
    let value: serde_json::Value = serde_json::from_str(&raw).map_err(|e| {
        LlmError::ProviderUnavailable(format!(
            "parse GOOGLE_APPLICATION_CREDENTIALS file {path}: {e}"
        ))
    })?;
    let credential_type = value
        .get("type")
        .and_then(|value| value.as_str())
        .unwrap_or_default();
    match credential_type {
        "service_account" => serde_json::from_value(value)
            .map(GoogleApplicationCredentials::ServiceAccount)
            .map_err(|e| {
                LlmError::ProviderUnavailable(format!(
                    "parse service_account GOOGLE_APPLICATION_CREDENTIALS file {path}: {e}"
                ))
            }),
        "authorized_user" => serde_json::from_value(value)
            .map(GoogleApplicationCredentials::AuthorizedUser)
            .map_err(|e| {
                LlmError::ProviderUnavailable(format!(
                    "parse authorized_user GOOGLE_APPLICATION_CREDENTIALS file {path}: {e}"
                ))
            }),
        other => Err(LlmError::ProviderUnavailable(format!(
            "unsupported GOOGLE_APPLICATION_CREDENTIALS type '{other}' in {path}"
        ))),
    }
}

async fn fetch_service_account_token(
    client: &reqwest::Client,
    credentials: &ServiceAccountCredentials,
) -> Result<AccessTokenResponse, LlmError> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| LlmError::ProviderUnavailable(format!("system clock before epoch: {e}")))?
        .as_secs() as usize;
    let claims = ServiceAccountJwtClaims {
        iss: &credentials.client_email,
        scope: "https://www.googleapis.com/auth/cloud-platform",
        aud: &credentials.token_uri,
        iat: now,
        exp: now + 3600,
    };
    let key = EncodingKey::from_rsa_pem(credentials.private_key.as_bytes()).map_err(|e| {
        LlmError::ProviderUnavailable(format!("decode service account private key: {e}"))
    })?;
    let assertion = encode(&Header::new(Algorithm::RS256), &claims, &key)
        .map_err(|e| LlmError::ProviderUnavailable(format!("sign service account jwt: {e}")))?;

    let response = client
        .post(&credentials.token_uri)
        .form(&ServiceAccountTokenRequest {
            grant_type: "urn:ietf:params:oauth:grant-type:jwt-bearer",
            assertion: &assertion,
        })
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .map_err(|e| LlmError::ProviderUnavailable(format!("service account token HTTP: {e}")))?;

    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(LlmError::ProviderUnavailable(format!(
            "service account token endpoint {}: {body}",
            status.as_u16()
        )));
    }

    response.json().await.map_err(|e| {
        LlmError::ProviderUnavailable(format!("service account token parse: {e}"))
    })
}

async fn fetch_authorized_user_token(
    client: &reqwest::Client,
    credentials: &AuthorizedUserCredentials,
) -> Result<AccessTokenResponse, LlmError> {
    let response = client
        .post(&credentials.token_uri)
        .form(&AuthorizedUserTokenRequest {
            client_id: &credentials.client_id,
            client_secret: &credentials.client_secret,
            refresh_token: &credentials.refresh_token,
            grant_type: "refresh_token",
        })
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .map_err(|e| LlmError::ProviderUnavailable(format!("authorized user token HTTP: {e}")))?;

    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(LlmError::ProviderUnavailable(format!(
            "authorized user token endpoint {}: {body}",
            status.as_u16()
        )));
    }

    response.json().await.map_err(|e| {
        LlmError::ProviderUnavailable(format!("authorized user token parse: {e}"))
    })
}

async fn fetch_impersonated_service_account_token(
    client: &reqwest::Client,
    source_access_token: &str,
    url: &str,
) -> Result<CachedToken, LlmError> {
    let scopes = ["https://www.googleapis.com/auth/cloud-platform"];
    let response = client
        .post(url)
        .bearer_auth(source_access_token)
        .json(&ImpersonatedTokenRequest {
            scope: &scopes,
            lifetime: "3600s",
        })
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .map_err(|e| LlmError::ProviderUnavailable(format!("service account impersonation HTTP: {e}")))?;

    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(LlmError::ProviderUnavailable(format!(
            "service account impersonation endpoint {}: {body}",
            status.as_u16()
        )));
    }

    let body: ImpersonatedTokenResponse = response.json().await.map_err(|e| {
        LlmError::ProviderUnavailable(format!("service account impersonation parse: {e}"))
    })?;
    build_cached_token_from_expire_time(body.access_token, &body.expire_time)
}

async fn fetch_google_application_credentials_token(
    client: &reqwest::Client,
    path: &str,
) -> Result<CachedToken, LlmError> {
    match load_google_application_credentials(path)? {
        GoogleApplicationCredentials::ServiceAccount(credentials) => {
            let body = fetch_service_account_token(client, &credentials).await?;
            Ok(build_cached_token(body.access_token, body.expires_in))
        }
        GoogleApplicationCredentials::AuthorizedUser(credentials) => {
            let body = fetch_authorized_user_token(client, &credentials).await?;
            if let Some(url) = configured_impersonation_url(
                credentials.service_account_impersonation_url.as_deref(),
            ) {
                fetch_impersonated_service_account_token(client, &body.access_token, &url).await
            } else {
                Ok(build_cached_token(body.access_token, body.expires_in))
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Error types
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub(crate) enum LlmError {
    /// 429 or transient failure — eligible for failover.
    RateLimited(String),
    /// Provider could not be reached — eligible for failover.
    ProviderUnavailable(String),
    /// The request itself was bad — do NOT fail over.
    BadRequest(String),
    /// All providers exhausted.
    AllProvidersExhausted(String),
}

impl std::fmt::Display for LlmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RateLimited(msg) => write!(f, "rate limited: {msg}"),
            Self::ProviderUnavailable(msg) => write!(f, "provider unavailable: {msg}"),
            Self::BadRequest(msg) => write!(f, "bad request: {msg}"),
            Self::AllProvidersExhausted(msg) => write!(f, "all providers exhausted: {msg}"),
        }
    }
}

impl LlmError {
    fn is_failover_eligible(&self) -> bool {
        matches!(self, Self::RateLimited(_) | Self::ProviderUnavailable(_))
    }
}

// ---------------------------------------------------------------------------
// Generative AI client
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct GeminiRequest {
    #[serde(rename = "systemInstruction", skip_serializing_if = "Option::is_none")]
    system_instruction: Option<GeminiContent>,
    contents: Vec<GeminiContent>,
    #[serde(rename = "generationConfig", skip_serializing_if = "Option::is_none")]
    generation_config: Option<GeminiGenerationConfig>,
}

#[derive(Serialize)]
struct GeminiContent {
    #[serde(skip_serializing_if = "Option::is_none")]
    role: Option<String>,
    parts: Vec<GeminiPart>,
}

#[derive(Serialize)]
struct GeminiPart {
    text: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct GeminiGenerationConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    max_output_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    response_mime_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    response_modalities: Option<Vec<String>>,
}

#[derive(Deserialize)]
struct GeminiResponse {
    candidates: Option<Vec<GeminiCandidate>>,
}

#[derive(Deserialize)]
struct GeminiCandidate {
    content: Option<GeminiResponseContent>,
}

#[derive(Deserialize)]
struct GeminiResponseContent {
    parts: Option<Vec<GeminiResponsePart>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GeminiResponsePart {
    text: Option<String>,
    inline_data: Option<GeminiInlineData>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GeminiInlineData {
    mime_type: Option<String>,
    data: Option<String>,
}

fn gemini_text_content(role: Option<&str>, text: &str) -> GeminiContent {
    GeminiContent {
        role: role.map(str::to_string),
        parts: vec![GeminiPart {
            text: text.to_string(),
        }],
    }
}

fn vertex_ai_generate_content_url(project: &str, location: &str, model: &str) -> String {
    let host = if location == "global" {
        "aiplatform.googleapis.com".to_string()
    } else {
        format!("{location}-aiplatform.googleapis.com")
    };

    format!(
        "https://{host}/v1/projects/{project}/locations/{location}/publishers/google/models/{model}:generateContent"
    )
}

fn vertex_ai_request_locations<'a>(location: &'a str) -> Vec<&'a str> {
    if location == "global" {
        vec![location]
    } else {
        vec![location, "global"]
    }
}

// ---------------------------------------------------------------------------
// Provider pool executor
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub(crate) struct LlmClient {
    config: Arc<LlmPoolConfig>,
    http: reqwest::Client,
    token_cache: GceTokenCache,
}

impl LlmClient {
    pub(crate) fn new(config: LlmPoolConfig) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(60))
            .build()
            .expect("build reqwest client");
        let token_cache = GceTokenCache::new(http.clone());
        Self {
            config: Arc::new(config),
            http,
            token_cache,
        }
    }

    // -----------------------------------------------------------------------
    // Judge
    // -----------------------------------------------------------------------

    pub(crate) async fn judge(&self, bundle: &JudgeBundle) -> Result<LlmJudgeEvaluation, LlmError> {
        if self.config.judge_providers.is_empty() {
            return Err(LlmError::BadRequest(
                "No judge providers configured.".to_string(),
            ));
        }

        let system_instruction = build_judge_system_instruction();
        let prompt = build_judge_user_prompt(bundle);
        let mut last_error = String::new();

        for (i, provider) in self.config.judge_providers.iter().enumerate() {
            info!(
                provider_index = i,
                provider_kind = ?provider.kind,
                model = %provider.model,
                "attempting judge provider"
            );

            match self
                .call_text_generation(provider, Some(system_instruction), &prompt)
                .await
            {
                Ok(text) => match parse_and_validate_judge_response(&text, bundle) {
                    Ok(evaluation) => return Ok(evaluation),
                    Err(e) => {
                        warn!(
                            provider_index = i,
                            error = %e,
                            "judge provider returned unparseable response, failing over"
                        );
                        last_error = e.to_string();
                        continue;
                    }
                },
                Err(e) if e.is_failover_eligible() => {
                    warn!(
                        provider_index = i,
                        error = %e,
                        "judge provider failed, failing over"
                    );
                    last_error = e.to_string();
                    continue;
                }
                Err(e) => return Err(e),
            }
        }

        Err(LlmError::AllProvidersExhausted(format!(
            "all {} judge providers failed; last error: {last_error}",
            self.config.judge_providers.len()
        )))
    }

    // -----------------------------------------------------------------------
    // Image generation
    // -----------------------------------------------------------------------

    pub(crate) async fn generate_image(&self, prompt: &str) -> Result<(String, String), LlmError> {
        self.generate_image_with_system_instruction(None, prompt)
            .await
    }

    async fn generate_image_with_system_instruction(
        &self,
        system_instruction: Option<&str>,
        prompt: &str,
    ) -> Result<(String, String), LlmError> {
        if self.config.image_providers.is_empty() {
            return Err(LlmError::BadRequest(
                "No image providers configured.".to_string(),
            ));
        }

        let mut last_error = String::new();

        for (i, provider) in self.config.image_providers.iter().enumerate() {
            info!(
                provider_index = i,
                provider_kind = ?provider.kind,
                model = %provider.model,
                "attempting image provider"
            );

            match self
                .call_image_generation(provider, system_instruction, prompt)
                .await
            {
                Ok((base64, mime)) => return Ok((base64, mime)),
                Err(e) if e.is_failover_eligible() => {
                    warn!(
                        provider_index = i,
                        error = %e,
                        "image provider failed, failing over"
                    );
                    last_error = e.to_string();
                    continue;
                }
                Err(e) => return Err(e),
            }
        }

        Err(LlmError::AllProvidersExhausted(format!(
            "all {} image providers failed; last error: {last_error}",
            self.config.image_providers.len()
        )))
    }

    // -----------------------------------------------------------------------
    // Sprite sheet generation: prompt → 4x2 grid → 8 base64 frames
    // -----------------------------------------------------------------------

    pub(crate) async fn generate_sprite_sheet(
        &self,
        description: &str,
    ) -> Result<SpriteSet, LlmError> {
        let system_instruction = build_sprite_sheet_system_instruction();
        let prompt = build_sprite_sheet_user_prompt(description);
        let (image_base64, _mime) = self
            .generate_image_with_system_instruction(Some(system_instruction), &prompt)
            .await?;
        slice_sprite_sheet(&image_base64)
    }

    // -----------------------------------------------------------------------
    // Internal: text generation (Gemini)
    // -----------------------------------------------------------------------

    async fn call_text_generation(
        &self,
        provider: &ResolvedProvider,
        system_instruction: Option<&str>,
        prompt: &str,
    ) -> Result<String, LlmError> {
        let request_body = GeminiRequest {
            system_instruction: system_instruction.map(|text| gemini_text_content(None, text)),
            contents: vec![gemini_text_content(Some("user"), prompt)],
            generation_config: Some(GeminiGenerationConfig {
                max_output_tokens: Some(4096),
                temperature: Some(0.2),
                response_mime_type: Some("application/json".to_string()),
                response_modalities: None,
            }),
        };

        let response = match provider.kind {
            LlmProviderKind::VertexAi => {
                let project = self.config.google_cloud_project.as_deref().ok_or_else(|| {
                    LlmError::BadRequest("GOOGLE_CLOUD_PROJECT required for vertex_ai".into())
                })?;
                let location = self
                    .config
                    .google_cloud_location
                    .as_deref()
                    .ok_or_else(|| {
                        LlmError::BadRequest("GOOGLE_CLOUD_LOCATION required for vertex_ai".into())
                    })?;
                let token = self.token_cache.get_token().await?;
                for request_location in vertex_ai_request_locations(location) {
                    let url =
                        vertex_ai_generate_content_url(project, request_location, &provider.model);
                    let response = match self
                        .http
                        .post(&url)
                        .bearer_auth(&token)
                        .json(&request_body)
                        .send()
                        .await
                    {
                        Ok(response) => response,
                        Err(e) => {
                            if request_location != "global" {
                                warn!(
                                    model = %provider.model,
                                    location = request_location,
                                    fallback_location = "global",
                                    error = %e,
                                    "vertex ai text request failed, retrying with global endpoint"
                                );
                                continue;
                            }
                            return Err(LlmError::ProviderUnavailable(format!("HTTP error: {e}")));
                        }
                    };

                    let status = response.status();
                    if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                        if request_location != "global" {
                            warn!(
                                model = %provider.model,
                                location = request_location,
                                fallback_location = "global",
                                "vertex ai text request rate limited, retrying with global endpoint"
                            );
                            continue;
                        }
                        return Err(LlmError::RateLimited("429 from vertex_ai provider".into()));
                    }
                    if status.is_server_error() {
                        if request_location != "global" {
                            warn!(
                                model = %provider.model,
                                location = request_location,
                                fallback_location = "global",
                                status = status.as_u16(),
                                "vertex ai text request hit server error, retrying with global endpoint"
                            );
                            continue;
                        }
                        return Err(LlmError::ProviderUnavailable(format!(
                            "{} server error",
                            status.as_u16()
                        )));
                    }
                    if !status.is_success() {
                        let body = response.text().await.unwrap_or_default();
                        return Err(LlmError::ProviderUnavailable(format!(
                            "{}: {body}",
                            status.as_u16()
                        )));
                    }

                    let body: GeminiResponse = response.json().await.map_err(|e| {
                        LlmError::ProviderUnavailable(format!("response parse: {e}"))
                    })?;

                    let text = body
                        .candidates
                        .as_ref()
                        .and_then(|c| c.first())
                        .and_then(|c| c.content.as_ref())
                        .and_then(|c| c.parts.as_ref())
                        .and_then(|p| p.first())
                        .and_then(|p| p.text.as_ref())
                        .ok_or_else(|| {
                            LlmError::ProviderUnavailable("empty response from model".to_string())
                        })?;

                    return Ok(text.clone());
                }

                return Err(LlmError::RateLimited("429 from vertex_ai provider".into()));
            }
            LlmProviderKind::ApiKey => {
                let api_key = provider.api_key.as_deref().ok_or_else(|| {
                    LlmError::BadRequest("API key missing for api_key provider".into())
                })?;
                let url = format!(
                    "https://generativelanguage.googleapis.com/v1beta/models/{}:generateContent",
                    provider.model
                );
                self.http
                    .post(&url)
                    .query(&[("key", api_key)])
                    .json(&request_body)
                    .send()
                    .await
            }
        };

        let response =
            response.map_err(|e| LlmError::ProviderUnavailable(format!("HTTP error: {e}")))?;

        let status = response.status();
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            return Err(LlmError::RateLimited(format!(
                "429 from {} provider",
                match provider.kind {
                    LlmProviderKind::VertexAi => "vertex_ai",
                    LlmProviderKind::ApiKey => "api_key",
                }
            )));
        }
        if status.is_server_error() {
            return Err(LlmError::ProviderUnavailable(format!(
                "{} server error",
                status.as_u16()
            )));
        }
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(LlmError::ProviderUnavailable(format!(
                "{}: {body}",
                status.as_u16()
            )));
        }

        let body: GeminiResponse = response
            .json()
            .await
            .map_err(|e| LlmError::ProviderUnavailable(format!("response parse: {e}")))?;

        let text = body
            .candidates
            .as_ref()
            .and_then(|c| c.first())
            .and_then(|c| c.content.as_ref())
            .and_then(|c| c.parts.as_ref())
            .and_then(|p| p.first())
            .and_then(|p| p.text.as_ref())
            .ok_or_else(|| {
                LlmError::ProviderUnavailable("empty response from model".to_string())
            })?;

        Ok(text.clone())
    }

    // -----------------------------------------------------------------------
    // Internal: image generation
    // -----------------------------------------------------------------------

    async fn call_image_generation(
        &self,
        provider: &ResolvedProvider,
        system_instruction: Option<&str>,
        prompt: &str,
    ) -> Result<(String, String), LlmError> {
        match provider.kind {
            LlmProviderKind::VertexAi => {
                self.call_vertex_ai_image(provider, system_instruction, prompt)
                    .await
            }
            LlmProviderKind::ApiKey => {
                self.call_api_key_image(provider, system_instruction, prompt)
                    .await
            }
        }
    }

    async fn call_vertex_ai_image(
        &self,
        provider: &ResolvedProvider,
        system_instruction: Option<&str>,
        prompt: &str,
    ) -> Result<(String, String), LlmError> {
        let project = self.config.google_cloud_project.as_deref().ok_or_else(|| {
            LlmError::BadRequest("GOOGLE_CLOUD_PROJECT required for vertex_ai".into())
        })?;
        let location = self
            .config
            .google_cloud_location
            .as_deref()
            .ok_or_else(|| {
                LlmError::BadRequest("GOOGLE_CLOUD_LOCATION required for vertex_ai".into())
            })?;
        let token = self.token_cache.get_token().await?;

        let request_body = GeminiRequest {
            system_instruction: system_instruction.map(|text| gemini_text_content(None, text)),
            contents: vec![gemini_text_content(Some("user"), prompt)],
            generation_config: Some(GeminiGenerationConfig {
                max_output_tokens: None,
                temperature: Some(0.7),
                response_mime_type: None,
                response_modalities: Some(vec!["IMAGE".to_string()]),
            }),
        };

        for request_location in vertex_ai_request_locations(location) {
            let url = vertex_ai_generate_content_url(project, request_location, &provider.model);
            let response = match self
                .http
                .post(&url)
                .bearer_auth(&token)
                .json(&request_body)
                .send()
                .await
            {
                Ok(response) => response,
                Err(e) => {
                    if request_location != "global" {
                        warn!(
                            model = %provider.model,
                            location = request_location,
                            fallback_location = "global",
                            error = %e,
                            "vertex ai image request failed, retrying with global endpoint"
                        );
                        continue;
                    }
                    return Err(LlmError::ProviderUnavailable(format!("HTTP error: {e}")));
                }
            };

            let status = response.status();
            if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                if request_location != "global" {
                    warn!(
                        model = %provider.model,
                        location = request_location,
                        fallback_location = "global",
                        "vertex ai image rate limited, retrying with global endpoint"
                    );
                    continue;
                }
                return Err(LlmError::RateLimited("429 from vertex_ai image".into()));
            }
            if status.is_server_error() {
                if request_location != "global" {
                    warn!(
                        model = %provider.model,
                        location = request_location,
                        fallback_location = "global",
                        status = status.as_u16(),
                        "vertex ai image request hit server error, retrying with global endpoint"
                    );
                    continue;
                }
                return Err(LlmError::ProviderUnavailable(format!(
                    "{} server error",
                    status.as_u16()
                )));
            }
            if !status.is_success() {
                let body = response.text().await.unwrap_or_default();
                return Err(LlmError::ProviderUnavailable(format!(
                    "{}: {body}",
                    status.as_u16()
                )));
            }

            let body: GeminiResponse = response
                .json()
                .await
                .map_err(|e| LlmError::ProviderUnavailable(format!("response parse: {e}")))?;

            return extract_gemini_image(&body);
        }

        Err(LlmError::RateLimited("429 from vertex_ai image".into()))
    }

    async fn call_api_key_image(
        &self,
        provider: &ResolvedProvider,
        system_instruction: Option<&str>,
        prompt: &str,
    ) -> Result<(String, String), LlmError> {
        let api_key = provider
            .api_key
            .as_deref()
            .ok_or_else(|| LlmError::BadRequest("API key missing for api_key provider".into()))?;

        let url = format!(
            "https://generativelanguage.googleapis.com/v1beta/models/{}:generateContent",
            provider.model
        );

        let request_body = GeminiRequest {
            system_instruction: system_instruction.map(|text| gemini_text_content(None, text)),
            contents: vec![gemini_text_content(Some("user"), prompt)],
            generation_config: Some(GeminiGenerationConfig {
                max_output_tokens: None,
                temperature: Some(0.7),
                response_mime_type: None,
                response_modalities: Some(vec!["IMAGE".to_string()]),
            }),
        };

        let response = self
            .http
            .post(&url)
            .query(&[("key", api_key)])
            .json(&request_body)
            .send()
            .await
            .map_err(|e| LlmError::ProviderUnavailable(format!("HTTP error: {e}")))?;

        let status = response.status();
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            return Err(LlmError::RateLimited("429 from api_key image".into()));
        }
        if status.is_server_error() {
            return Err(LlmError::ProviderUnavailable(format!(
                "{} server error",
                status.as_u16()
            )));
        }
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(LlmError::ProviderUnavailable(format!(
                "{}: {body}",
                status.as_u16()
            )));
        }

        let body: GeminiResponse = response
            .json()
            .await
            .map_err(|e| LlmError::ProviderUnavailable(format!("response parse: {e}")))?;

        extract_gemini_image(&body)
    }
}

/// Extract base64 image data from a Gemini generateContent response.
fn extract_gemini_image(body: &GeminiResponse) -> Result<(String, String), LlmError> {
    let parts = body
        .candidates
        .as_ref()
        .and_then(|c| c.first())
        .and_then(|c| c.content.as_ref())
        .and_then(|c| c.parts.as_ref())
        .ok_or_else(|| LlmError::ProviderUnavailable("empty response from image model".into()))?;

    for part in parts {
        if let Some(inline) = &part.inline_data {
            if let Some(data) = &inline.data {
                let mime = inline
                    .mime_type
                    .as_deref()
                    .unwrap_or("image/png")
                    .to_string();
                return Ok((data.clone(), mime));
            }
        }
    }

    Err(LlmError::ProviderUnavailable(
        "no image data in model response".into(),
    ))
}

// ---------------------------------------------------------------------------
// Sprite sheet slicing: base64 → 4 emotion frames → SpriteSet
// ---------------------------------------------------------------------------

/// Layout: 2 columns × 2 rows.
/// Top-left: neutral, Top-right: happy
/// Bottom-left: angry, Bottom-right: sleepy
fn slice_sprite_sheet(image_base64: &str) -> Result<SpriteSet, LlmError> {
    use base64::Engine as _;
    use image::{GenericImageView, ImageFormat, ImageReader};
    use std::io::Cursor;

    let raw = base64::engine::general_purpose::STANDARD
        .decode(image_base64)
        .map_err(|e| LlmError::BadRequest(format!("failed to decode sprite sheet base64: {e}")))?;

    let img = ImageReader::with_format(Cursor::new(&raw), ImageFormat::Png)
        .decode()
        .or_else(|_| {
            // Fallback: let image crate guess format
            ImageReader::new(Cursor::new(&raw))
                .with_guessed_format()
                .map_err(|e| LlmError::BadRequest(format!("failed to guess image format: {e}")))?
                .decode()
                .map_err(|e| {
                    LlmError::BadRequest(format!("failed to decode sprite sheet image: {e}"))
                })
        })?;

    let (w, h) = img.dimensions();
    let tile_w = w / 2;
    let tile_h = h / 2;

    if tile_w == 0 || tile_h == 0 {
        return Err(LlmError::BadRequest(format!(
            "sprite sheet too small to slice into 2×2 grid: {w}×{h}"
        )));
    }

    let encode_tile = |col: u32, row: u32| -> Result<String, LlmError> {
        let tile = img.crop_imm(col * tile_w, row * tile_h, tile_w, tile_h);
        let mut buf = Vec::new();
        tile.write_to(&mut Cursor::new(&mut buf), ImageFormat::Png)
            .map_err(|e| LlmError::BadRequest(format!("failed to encode tile: {e}")))?;
        Ok(base64::engine::general_purpose::STANDARD.encode(&buf))
    };

    Ok(SpriteSet {
        neutral: encode_tile(0, 0)?,
        happy: encode_tile(1, 0)?,
        angry: encode_tile(0, 1)?,
        sleepy: encode_tile(1, 1)?,
    })
}

fn build_sprite_sheet_system_instruction() -> &'static str {
    r#"You are a production sprite-sheet generator for an automated game pipeline.

Your output will be sliced mechanically into 4 equal tiles, so layout accuracy is mandatory.

Hard requirements:
- Return exactly one image.
- The full canvas must be a 2 columns × 2 rows sprite sheet.
- Use equal-sized tiles with perfect alignment.
- No gutters, no spacing, no borders, no frames, no labels, no captions, no text.
- Show the same dragon in all 4 tiles.
- Keep the same camera angle, scale, framing, and silhouette in every tile.
- Center the dragon in each tile and let it fill most of the tile.
- Retro pixel-art only: crisp edges, no anti-aliasing, no painterly shading, no blur.
- Background must be transparent or a uniform dark flat background.
- No props, scenery, UI, speech bubbles, or extra characters.

Emotion order is fixed:
- Top row, left to right: neutral, happy
- Bottom row, left to right: angry, sleepy

Positive example of the required layout:
- [neutral][happy]
- [angry][sleepy]"#
}

fn build_sprite_sheet_user_prompt(description: &str) -> String {
    format!(
        r#"Create a single sprite sheet image that follows the system instructions.

Dragon description: {description}

Render the same dragon across 4 emotion tiles with clearly distinct facial expression and body language.
Do not add any text or decorative frame elements.
Return exactly one image."#
    )
}

// ---------------------------------------------------------------------------
// Judge prompt builder
// ---------------------------------------------------------------------------

fn build_judge_system_instruction() -> &'static str {
    r#"You are a deterministic scoring service for Dragon Care Workshop.

The game has one dragon bundle per created dragon. For each dragon:
- observationScore is awarded to the creator / Phase 1 observer
- careScore is awarded to the current owner / Phase 2 caretaker
- creativityScore is a separate descriptive dimension for design/personality flavor and should be scored independently from the mechanics-focused axes

Hard requirements:
- Evaluate EVERY dragon in the input exactly once.
- Return EXACTLY one object in dragonEvaluations for each input dragon.
- Preserve every dragonId exactly as provided in the input.
- Keep dragonEvaluations in the same order as the input dragons array.
- Return strict JSON only.
- Do not use markdown fences.
- Do not include commentary outside JSON.
- Use integer scores only.
- Keep all scores within 0..100.
- Use only the evidence provided in the input bundle.
- Ignore creativeVoteCount when assigning observationScore and careScore.
- Ignore existing player scores and achievements when assigning observationScore and careScore.
- If evidence is weak, contradictory, or missing, score conservatively instead of guessing.

Scoring rubric:
- observationScore (0-100)
  - 40 points: correctness of discovered hidden preferences
  - 25 points: usefulness and specificity of handover tags
  - 20 points: completeness across active time, food, and play
  - 15 points: clarity and lack of contradiction
- careScore (0-100)
  - 50 points: correct action ratio and alignment with handover tags
  - 20 points: finalStats quality
  - 15 points: low wrong-action and low-penalty behavior
  - 10 points: cooldown discipline
  - 5 points: reasonable recovery when handover quality is poor
- creativityScore (0-100)
  - informational only; evaluate how creative and entertaining the written observations and handover are

Output example:
{
  "summary": "The workshop showed strong discovery quality with mixed Phase 2 execution.",
  "dragonEvaluations": [
    {
      "dragonId": "dragon-1",
      "dragonName": "Comet",
      "observationScore": 82,
      "careScore": 74,
      "creativityScore": 68,
      "feedback": "The creator identified key preferences well, while the caretaker made a few avoidable mistakes."
    }
  ]
}"#
}

fn build_judge_user_prompt(bundle: &JudgeBundle) -> String {
    let bundle_json = serde_json::to_string_pretty(bundle).unwrap_or_default();
    let dragon_count = bundle.dragons.len();

    format!(
        r#"Evaluate the workshop bundle below.

Task:
- Return exactly {dragon_count} dragonEvaluations.
- Keep the same order as bundle.dragons.
- Preserve each dragonId exactly.
- Write a summary of 1-2 sentences.

Workshop data:
{bundle_json}"#
    )
}

// ---------------------------------------------------------------------------
// Response parser
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawJudgeResponse {
    summary: String,
    dragon_evaluations: Vec<RawDragonEvaluation>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawDragonEvaluation {
    dragon_id: String,
    dragon_name: String,
    observation_score: i32,
    care_score: i32,
    creativity_score: i32,
    feedback: String,
}

fn parse_judge_response(text: &str) -> Result<LlmJudgeEvaluation, String> {
    // Strip markdown code fences if present
    let cleaned = text
        .trim()
        .strip_prefix("```json")
        .or_else(|| text.trim().strip_prefix("```"))
        .unwrap_or(text.trim());
    let cleaned = cleaned.strip_suffix("```").unwrap_or(cleaned).trim();

    let raw: RawJudgeResponse =
        serde_json::from_str(cleaned).map_err(|e| format!("failed to parse judge JSON: {e}"))?;

    Ok(LlmJudgeEvaluation {
        summary: raw.summary,
        dragon_evaluations: raw
            .dragon_evaluations
            .into_iter()
            .map(|d| LlmDragonEvaluation {
                dragon_id: d.dragon_id,
                dragon_name: d.dragon_name,
                observation_score: d.observation_score.clamp(0, 100),
                care_score: d.care_score.clamp(0, 100),
                creativity_score: d.creativity_score.clamp(0, 100),
                feedback: d.feedback,
            })
            .collect(),
    })
}

fn parse_and_validate_judge_response(
    text: &str,
    bundle: &JudgeBundle,
) -> Result<LlmJudgeEvaluation, String> {
    let evaluation = parse_judge_response(text)?;
    validate_judge_response(&evaluation, bundle)?;
    Ok(evaluation)
}

fn validate_judge_response(
    evaluation: &LlmJudgeEvaluation,
    bundle: &JudgeBundle,
) -> Result<(), String> {
    if evaluation.summary.trim().is_empty() {
        return Err("judge summary is empty".to_string());
    }

    if evaluation.dragon_evaluations.len() != bundle.dragons.len() {
        return Err(format!(
            "expected {} dragon evaluations, got {}",
            bundle.dragons.len(),
            evaluation.dragon_evaluations.len()
        ));
    }

    for (index, (expected, actual)) in bundle
        .dragons
        .iter()
        .zip(evaluation.dragon_evaluations.iter())
        .enumerate()
    {
        if actual.dragon_id != expected.dragon_id {
            return Err(format!(
                "dragonEvaluations[{index}].dragonId must be '{}' but was '{}'",
                expected.dragon_id, actual.dragon_id
            ));
        }
        if actual.dragon_name.trim().is_empty() {
            return Err(format!(
                "dragonEvaluations[{index}].dragonName must not be empty"
            ));
        }
        if actual.feedback.trim().is_empty() {
            return Err(format!(
                "dragonEvaluations[{index}].feedback must not be empty"
            ));
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{LazyLock, Mutex as StdMutex, MutexGuard};

    struct ScopedEnvVar {
        key: &'static str,
        original: Option<String>,
        _guard: MutexGuard<'static, ()>,
    }

    static ENV_TEST_MUTEX: LazyLock<StdMutex<()>> = LazyLock::new(|| StdMutex::new(()));

    impl ScopedEnvVar {
        fn set(key: &'static str, value: &str) -> Self {
            let guard = ENV_TEST_MUTEX.lock().expect("lock llm env test mutex");
            let original = std::env::var(key).ok();
            unsafe {
                std::env::set_var(key, value);
            }
            Self {
                key,
                original,
                _guard: guard,
            }
        }
    }

    impl Drop for ScopedEnvVar {
        fn drop(&mut self) {
            if let Some(original) = &self.original {
                unsafe {
                    std::env::set_var(self.key, original);
                }
            } else {
                unsafe {
                    std::env::remove_var(self.key);
                }
            }
        }
    }

    fn mock_judge_bundle() -> JudgeBundle {
        JudgeBundle {
            session_id: "session-1".to_string(),
            session_code: "123456".to_string(),
            current_phase: protocol::Phase::End,
            generated_at: "2026-01-01T12:00:00Z".to_string(),
            artifact_count: 2,
            players: vec![],
            dragons: vec![
                protocol::JudgeDragonBundle {
                    dragon_id: "dragon-1".to_string(),
                    dragon_name: "Comet".to_string(),
                    creator_player_id: "player-1".to_string(),
                    creator_name: "Alice".to_string(),
                    current_owner_id: "player-2".to_string(),
                    current_owner_name: "Bob".to_string(),
                    creative_vote_count: 1,
                    final_stats: protocol::DragonStats {
                        hunger: 70,
                        energy: 65,
                        happiness: 80,
                    },
                    actual_active_time: protocol::ActiveTime::Day,
                    actual_day_food: protocol::FoodType::Meat,
                    actual_night_food: protocol::FoodType::Fruit,
                    actual_day_play: protocol::PlayType::Fetch,
                    actual_night_play: protocol::PlayType::Puzzle,
                    actual_sleep_rate: 2,
                    handover_chain: protocol::JudgeHandoverChain {
                        creator_instructions: "Feed at dusk".to_string(),
                        discovery_observations: vec!["Likes meat by day".to_string()],
                        handover_tags: vec!["Feed at dusk".to_string()],
                    },
                    phase2_actions: vec![],
                    total_actions: 4,
                    correct_actions: 3,
                    wrong_food_count: 1,
                    wrong_play_count: 0,
                    cooldown_violations: 0,
                    penalty_stacks_at_end: 0,
                    phase2_lowest_happiness: 66,
                },
                protocol::JudgeDragonBundle {
                    dragon_id: "dragon-2".to_string(),
                    dragon_name: "Nova".to_string(),
                    creator_player_id: "player-2".to_string(),
                    creator_name: "Bob".to_string(),
                    current_owner_id: "player-1".to_string(),
                    current_owner_name: "Alice".to_string(),
                    creative_vote_count: 2,
                    final_stats: protocol::DragonStats {
                        hunger: 55,
                        energy: 72,
                        happiness: 74,
                    },
                    actual_active_time: protocol::ActiveTime::Night,
                    actual_day_food: protocol::FoodType::Fish,
                    actual_night_food: protocol::FoodType::Meat,
                    actual_day_play: protocol::PlayType::Music,
                    actual_night_play: protocol::PlayType::Fetch,
                    actual_sleep_rate: 1,
                    handover_chain: protocol::JudgeHandoverChain {
                        creator_instructions: "Play music during the day".to_string(),
                        discovery_observations: vec!["Settles with music".to_string()],
                        handover_tags: vec!["Play music during the day".to_string()],
                    },
                    phase2_actions: vec![],
                    total_actions: 5,
                    correct_actions: 4,
                    wrong_food_count: 0,
                    wrong_play_count: 1,
                    cooldown_violations: 1,
                    penalty_stacks_at_end: 1,
                    phase2_lowest_happiness: 61,
                },
            ],
        }
    }

    #[test]
    fn parse_judge_response_handles_clean_json() {
        let json = r#"{
            "summary": "Good session.",
            "dragonEvaluations": [{
                "dragonId": "dragon_1",
                "dragonName": "Sparky",
                "observationScore": 60,
                "careScore": 85,
                "creativityScore": 70,
                "feedback": "Well cared for."
            }]
        }"#;

        let eval = parse_judge_response(json).expect("parse");
        assert_eq!(eval.summary, "Good session.");
        assert_eq!(eval.dragon_evaluations.len(), 1);
        assert_eq!(eval.dragon_evaluations[0].observation_score, 60);
        assert_eq!(eval.dragon_evaluations[0].care_score, 85);
    }

    #[test]
    fn parse_judge_response_strips_markdown_fences() {
        let json = r#"```json
{
    "summary": "Test.",
    "dragonEvaluations": []
}
```"#;

        let eval = parse_judge_response(json).expect("parse");
        assert_eq!(eval.summary, "Test.");
    }

    #[test]
    fn parse_judge_response_clamps_scores() {
        let json = r#"{
            "summary": "Test.",
            "dragonEvaluations": [{
                "dragonId": "d1",
                "dragonName": "X",
                "observationScore": 200,
                "careScore": 150,
                "creativityScore": -10,
                "feedback": "ok"
            }]
        }"#;

        let eval = parse_judge_response(json).expect("parse");
        assert_eq!(eval.dragon_evaluations[0].observation_score, 100);
        assert_eq!(eval.dragon_evaluations[0].care_score, 100);
        assert_eq!(eval.dragon_evaluations[0].creativity_score, 0);
    }

    #[test]
    fn validate_judge_response_rejects_missing_dragon_rows() {
        let bundle = mock_judge_bundle();
        let json = r#"{
            "summary": "Test.",
            "dragonEvaluations": [{
                "dragonId": "dragon-1",
                "dragonName": "Comet",
                "observationScore": 60,
                "careScore": 80,
                "creativityScore": 50,
                "feedback": "Good."
            }]
        }"#;

        let error = parse_and_validate_judge_response(json, &bundle).expect_err("must fail");
        assert!(error.contains("expected 2 dragon evaluations, got 1"));
    }

    #[test]
    fn validate_judge_response_rejects_wrong_dragon_id_order() {
        let bundle = mock_judge_bundle();
        let json = r#"{
            "summary": "Test.",
            "dragonEvaluations": [
                {
                    "dragonId": "dragon-2",
                    "dragonName": "Nova",
                    "observationScore": 60,
                    "careScore": 80,
                    "creativityScore": 50,
                    "feedback": "Good."
                },
                {
                    "dragonId": "dragon-1",
                    "dragonName": "Comet",
                    "observationScore": 70,
                    "careScore": 75,
                    "creativityScore": 55,
                    "feedback": "Good."
                }
            ]
        }"#;

        let error = parse_and_validate_judge_response(json, &bundle).expect_err("must fail");
        assert!(error.contains("dragonEvaluations[0].dragonId must be 'dragon-1'"));
    }

    #[test]
    fn validate_judge_response_accepts_exact_bundle_order() {
        let bundle = mock_judge_bundle();
        let json = r#"{
            "summary": "Solid discovery, decent care.",
            "dragonEvaluations": [
                {
                    "dragonId": "dragon-1",
                    "dragonName": "Comet",
                    "observationScore": 60,
                    "careScore": 80,
                    "creativityScore": 50,
                    "feedback": "Good handover quality."
                },
                {
                    "dragonId": "dragon-2",
                    "dragonName": "Nova",
                    "observationScore": 70,
                    "careScore": 75,
                    "creativityScore": 55,
                    "feedback": "Mostly correct care decisions."
                }
            ]
        }"#;

        let evaluation = parse_and_validate_judge_response(json, &bundle).expect("must pass");
        assert_eq!(evaluation.dragon_evaluations.len(), 2);
        assert_eq!(evaluation.dragon_evaluations[0].dragon_id, "dragon-1");
        assert_eq!(evaluation.dragon_evaluations[1].dragon_id, "dragon-2");
    }

    #[test]
    fn sprite_sheet_prompts_encode_hard_layout_rules() {
        let system = build_sprite_sheet_system_instruction();
        let user = build_sprite_sheet_user_prompt("violet crystal dragon");

        assert!(system.contains("2 columns × 2 rows sprite sheet"));
        assert!(system.contains("No gutters, no spacing, no borders"));
        assert!(system.contains("Top row, left to right: neutral, happy"));
        assert!(system.contains("Bottom row, left to right: angry, sleepy"));
        assert!(user.contains("Dragon description: violet crystal dragon"));
        assert!(user.contains("Render the same dragon across 4 emotion tiles"));
    }

    #[test]
    fn judge_prompts_encode_deterministic_contract() {
        let bundle = mock_judge_bundle();
        let system = build_judge_system_instruction();
        let user = build_judge_user_prompt(&bundle);

        assert!(system.contains("Evaluate EVERY dragon in the input exactly once"));
        assert!(system.contains("creativityScore is a separate descriptive dimension"));
        assert!(user.contains("Return exactly 2 dragonEvaluations"));
        assert!(user.contains("Preserve each dragonId exactly"));
    }

    #[test]
    fn vertex_ai_generate_content_url_supports_regional_and_global_locations() {
        assert_eq!(
            vertex_ai_generate_content_url("proj-1", "europe-west4", "gemini-2.5-flash-image"),
            "https://europe-west4-aiplatform.googleapis.com/v1/projects/proj-1/locations/europe-west4/publishers/google/models/gemini-2.5-flash-image:generateContent"
        );
        assert_eq!(
            vertex_ai_generate_content_url("proj-1", "global", "gemini-2.5-flash-image"),
            "https://aiplatform.googleapis.com/v1/projects/proj-1/locations/global/publishers/google/models/gemini-2.5-flash-image:generateContent"
        );
    }

    #[test]
    fn vertex_ai_request_locations_adds_global_fallback_for_regional_endpoints() {
        assert_eq!(
            vertex_ai_request_locations("europe-west4"),
            vec!["europe-west4", "global"]
        );
        assert_eq!(vertex_ai_request_locations("global"), vec!["global"]);
    }

    #[test]
    fn resolve_providers_skips_api_key_missing_env() {
        let entries = vec![LlmProviderEntry {
            provider_type: LlmProviderKind::ApiKey,
            model: "test-model".to_string(),
            api_key_env_var: Some("NONEXISTENT_KEY_12345".to_string()),
        }];

        let resolved = resolve_providers(&entries, "test");
        assert!(resolved.is_empty());
    }

    #[test]
    fn resolve_providers_keeps_vertex_ai_without_key() {
        let entries = vec![LlmProviderEntry {
            provider_type: LlmProviderKind::VertexAi,
            model: "gemini-1.5-pro".to_string(),
            api_key_env_var: None,
        }];

        let resolved = resolve_providers(&entries, "test");
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].model, "gemini-1.5-pro");
    }

    #[test]
    fn configured_impersonation_url_prefers_explicit_env() {
        let _impersonate = ScopedEnvVar::set(
            "GOOGLE_IMPERSONATE_SERVICE_ACCOUNT",
            "dragon-shift-local-kube@rna-workshop.iam.gserviceaccount.com",
        );

        let url = configured_impersonation_url(Some(
            "https://iamcredentials.googleapis.com/v1/projects/-/serviceAccounts/ignored:generateAccessToken",
        ));

        assert_eq!(
            url.as_deref(),
            Some(
                "https://iamcredentials.googleapis.com/v1/projects/-/serviceAccounts/dragon-shift-local-kube@rna-workshop.iam.gserviceaccount.com:generateAccessToken"
            )
        );
    }

    #[test]
    fn configured_impersonation_url_uses_adc_value_when_env_missing() {
        let _impersonate = ScopedEnvVar::set("GOOGLE_IMPERSONATE_SERVICE_ACCOUNT", "");

        let url = configured_impersonation_url(Some(
            "https://iamcredentials.googleapis.com/v1/projects/-/serviceAccounts/from-adc:generateAccessToken",
        ));

        assert_eq!(
            url.as_deref(),
            Some(
                "https://iamcredentials.googleapis.com/v1/projects/-/serviceAccounts/from-adc:generateAccessToken"
            )
        );
    }
}
