use protocol::{
    JudgeBundle, LlmDragonEvaluation, LlmJudgeEvaluation, LlmProviderEntry, LlmProviderKind,
};
use serde::{Deserialize, Serialize};
use std::{env, sync::Arc, time::Duration};
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
pub(crate) fn resolve_providers(
    entries: &[LlmProviderEntry],
    role: &str,
) -> Vec<ResolvedProvider> {
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
        return Err("GOOGLE_CLOUD_PROJECT is required when any LLM provider uses vertex_ai"
            .to_string());
    }

    if uses_vertex_ai && google_cloud_location.is_none() {
        return Err("GOOGLE_CLOUD_LOCATION is required when any LLM provider uses vertex_ai"
            .to_string());
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
struct MetadataTokenResponse {
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

#[derive(Clone)]
struct CachedToken {
    access_token: String,
    expires_at: std::time::Instant,
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

        let body: MetadataTokenResponse = response
            .json()
            .await
            .map_err(|e| LlmError::ProviderUnavailable(format!("metadata token parse: {e}")))?;

        let cached = CachedToken {
            access_token: body.access_token.clone(),
            expires_at: std::time::Instant::now() + Duration::from_secs(body.expires_in),
        };
        *self.token.lock().await = Some(cached);

        Ok(body.access_token)
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
    contents: Vec<GeminiContent>,
    #[serde(rename = "generationConfig", skip_serializing_if = "Option::is_none")]
    generation_config: Option<GeminiGenerationConfig>,
}

#[derive(Serialize)]
struct GeminiContent {
    role: String,
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
struct GeminiResponsePart {
    text: Option<String>,
}

// Imagen request/response
#[derive(Serialize)]
struct ImagenRequest {
    instances: Vec<ImagenInstance>,
    parameters: ImagenParameters,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ImagenInstance {
    prompt: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ImagenParameters {
    sample_count: u32,
}

#[derive(Deserialize)]
struct ImagenResponse {
    predictions: Option<Vec<ImagenPrediction>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ImagenPrediction {
    bytes_base64_encoded: Option<String>,
    mime_type: Option<String>,
}

// Generative Language API image response
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GenAiImageResponse {
    generated_images: Option<Vec<GenAiGeneratedImage>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GenAiGeneratedImage {
    image: Option<GenAiImageData>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GenAiImageData {
    image_bytes: Option<String>,
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

        let prompt = build_judge_prompt(bundle);
        let mut last_error = String::new();

        for (i, provider) in self.config.judge_providers.iter().enumerate() {
            info!(
                provider_index = i,
                provider_kind = ?provider.kind,
                model = %provider.model,
                "attempting judge provider"
            );

            match self
                .call_text_generation(provider, &prompt)
                .await
            {
                Ok(text) => match parse_judge_response(&text) {
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

    pub(crate) async fn generate_image(
        &self,
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

            match self.call_image_generation(provider, prompt).await {
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
    // Internal: text generation (Gemini)
    // -----------------------------------------------------------------------

    async fn call_text_generation(
        &self,
        provider: &ResolvedProvider,
        prompt: &str,
    ) -> Result<String, LlmError> {
        let request_body = GeminiRequest {
            contents: vec![GeminiContent {
                role: "user".to_string(),
                parts: vec![GeminiPart {
                    text: prompt.to_string(),
                }],
            }],
            generation_config: Some(GeminiGenerationConfig {
                max_output_tokens: Some(4096),
                temperature: Some(0.4),
                response_mime_type: Some("application/json".to_string()),
            }),
        };

        let response = match provider.kind {
            LlmProviderKind::VertexAi => {
                let project = self
                    .config
                    .google_cloud_project
                    .as_deref()
                    .ok_or_else(|| {
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
                let url = format!(
                    "https://{location}-aiplatform.googleapis.com/v1/projects/{project}/locations/{location}/publishers/google/models/{}:generateContent",
                    provider.model
                );
                self.http
                    .post(&url)
                    .bearer_auth(&token)
                    .json(&request_body)
                    .send()
                    .await
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

        let response = response
            .map_err(|e| LlmError::ProviderUnavailable(format!("HTTP error: {e}")))?;

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
        prompt: &str,
    ) -> Result<(String, String), LlmError> {
        match provider.kind {
            LlmProviderKind::VertexAi => {
                self.call_vertex_ai_image(provider, prompt).await
            }
            LlmProviderKind::ApiKey => {
                self.call_api_key_image(provider, prompt).await
            }
        }
    }

    async fn call_vertex_ai_image(
        &self,
        provider: &ResolvedProvider,
        prompt: &str,
    ) -> Result<(String, String), LlmError> {
        let project = self
            .config
            .google_cloud_project
            .as_deref()
            .ok_or_else(|| {
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

        let url = format!(
            "https://{location}-aiplatform.googleapis.com/v1/projects/{project}/locations/{location}/publishers/google/models/{}:predict",
            provider.model
        );

        let request_body = ImagenRequest {
            instances: vec![ImagenInstance {
                prompt: prompt.to_string(),
            }],
            parameters: ImagenParameters { sample_count: 1 },
        };

        let response = self
            .http
            .post(&url)
            .bearer_auth(&token)
            .json(&request_body)
            .send()
            .await
            .map_err(|e| LlmError::ProviderUnavailable(format!("HTTP error: {e}")))?;

        let status = response.status();
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            return Err(LlmError::RateLimited("429 from vertex_ai image".into()));
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

        let body: ImagenResponse = response
            .json()
            .await
            .map_err(|e| LlmError::ProviderUnavailable(format!("response parse: {e}")))?;

        let prediction = body
            .predictions
            .as_ref()
            .and_then(|p| p.first())
            .ok_or_else(|| LlmError::ProviderUnavailable("no predictions returned".into()))?;

        let base64 = prediction
            .bytes_base64_encoded
            .as_ref()
            .ok_or_else(|| LlmError::ProviderUnavailable("no image bytes returned".into()))?;
        let mime = prediction
            .mime_type
            .as_deref()
            .unwrap_or("image/png")
            .to_string();

        Ok((base64.clone(), mime))
    }

    async fn call_api_key_image(
        &self,
        provider: &ResolvedProvider,
        prompt: &str,
    ) -> Result<(String, String), LlmError> {
        let api_key = provider.api_key.as_deref().ok_or_else(|| {
            LlmError::BadRequest("API key missing for api_key provider".into())
        })?;

        // Use Gemini generateContent with image generation instructions
        // as the Generative Language API for image models varies.
        // For Imagen models through the API key path, use the predict endpoint
        // style adapted for the Generative Language API.
        let url = format!(
            "https://generativelanguage.googleapis.com/v1beta/models/{}:predict",
            provider.model
        );

        let request_body = ImagenRequest {
            instances: vec![ImagenInstance {
                prompt: prompt.to_string(),
            }],
            parameters: ImagenParameters { sample_count: 1 },
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

        // Try Imagen-style response first, then GenAI-style
        let body_text = response
            .text()
            .await
            .map_err(|e| LlmError::ProviderUnavailable(format!("response read: {e}")))?;

        if let Ok(imagen_resp) = serde_json::from_str::<ImagenResponse>(&body_text) {
            if let Some(prediction) = imagen_resp.predictions.as_ref().and_then(|p| p.first()) {
                if let Some(base64) = &prediction.bytes_base64_encoded {
                    let mime = prediction
                        .mime_type
                        .as_deref()
                        .unwrap_or("image/png")
                        .to_string();
                    return Ok((base64.clone(), mime));
                }
            }
        }

        if let Ok(genai_resp) = serde_json::from_str::<GenAiImageResponse>(&body_text) {
            if let Some(img) = genai_resp
                .generated_images
                .as_ref()
                .and_then(|imgs| imgs.first())
            {
                if let Some(base64) = img
                    .image
                    .as_ref()
                    .and_then(|i| i.image_bytes.as_ref())
                {
                    return Ok((base64.clone(), "image/png".to_string()));
                }
            }
        }

        Err(LlmError::ProviderUnavailable(
            "could not extract image from provider response".into(),
        ))
    }
}

// ---------------------------------------------------------------------------
// Judge prompt builder
// ---------------------------------------------------------------------------

fn build_judge_prompt(bundle: &JudgeBundle) -> String {
    let bundle_json = serde_json::to_string_pretty(bundle).unwrap_or_default();

    format!(
        r#"You are the judge for a Dragon Care Workshop — a game where players create dragons with hidden preferences, observe them, and hand them over to a second caretaker.

Each dragon has SECRET preferences that the Phase 1 player must discover through experimentation:
- `actualActiveTime`: "day" or "night" — when the dragon is most active.
- `actualDayFood` / `actualNightFood`: preferred food during day vs night ("meat", "fruit", or "fish").
- `actualDayPlay` / `actualNightPlay`: preferred play activity during day vs night ("fetch", "puzzle", or "music").
- `actualSleepRate`: how fast the dragon gets tired (1-3).

## Scoring criteria

For each dragon, produce TWO scores:

### observationScore (0-100) — Phase 1 quality
Awarded to the CREATOR (Phase 1 sitter). Evaluate:
1. Did their `discoveryObservations` accurately identify the dragon's real preferences?
2. Did their `handoverTags` contain useful, specific care instructions for the next sitter?
3. Reward thoroughness: identifying active time, correct food for each period, correct play for each period.
4. Penalize vague, incorrect, or missing observations.

### careScore (0-100) — Phase 2 quality
Awarded to the CURRENT OWNER (Phase 2 sitter). Evaluate:
1. Did their `phase2Actions` follow the `handoverTags` instructions from the Phase 1 player?
2. Did they feed the correct food, play the correct game, sleep at the right time?
3. Consider the `finalStats` — are hunger, energy, happiness in good shape?
4. If handover instructions were poor, give partial credit for reasonable independent care.
5. Use the summary stats to assess care quality:
   - `totalActions` vs `correctActions` — what percentage of actions were correct?
   - `wrongFoodCount` / `wrongPlayCount` — how many wrong choices were made?
   - `cooldownViolations` — did the player spam actions recklessly?
   - `penaltyStacksAtEnd` — were there accumulated penalties at game end?
   - `phase2LowestHappiness` — did happiness drop critically at any point?
   - Each action trace has `wasCorrect` (bool) and `blockReason` if blocked.
6. Penalize heavily for high cooldown violations (spam) and many wrong actions.
7. Reward players who achieved high correct-action ratios with few mistakes.

### creativityScore (0-100) — quality of descriptions
How creative and entertaining were the observations and handover tags?

Also write a 1-2 sentence overall summary of the workshop session.

Return ONLY valid JSON in this exact format:
{{
  "summary": "Overall session summary here.",
  "dragonEvaluations": [
    {{
      "dragonId": "dragon_id_here",
      "dragonName": "name_here",
      "observationScore": 75,
      "careScore": 85,
      "creativityScore": 70,
      "feedback": "Brief feedback here."
    }}
  ]
}}

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
    let cleaned = cleaned
        .strip_suffix("```")
        .unwrap_or(cleaned)
        .trim();

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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

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
}
