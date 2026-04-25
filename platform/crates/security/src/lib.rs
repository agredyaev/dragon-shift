use base64::Engine;
use std::collections::BTreeSet;
use thiserror::Error;
use url::Url;

pub const SESSION_CODE_LENGTH: usize = 6;
pub const DEFAULT_RUST_SESSION_CODE_PREFIX: &str = "9";
const PNG_SIGNATURE: [u8; 8] = [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a];

// Argon2id parameters per refactor-plan.md §1: m=19456 KiB / t=2 / p=1.
// Callers MUST invoke these helpers inside `tokio::task::spawn_blocking`
// because argon2 is CPU-bound and would otherwise stall the async runtime.
const ARGON2_MEMORY_KIB: u32 = 19_456;
const ARGON2_ITERATIONS: u32 = 2;
const ARGON2_PARALLELISM: u32 = 1;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum PasswordHashError {
    #[error("invalid argon2 parameters")]
    InvalidParams,
    #[error("failed to hash password")]
    HashFailure,
    #[error("stored hash is malformed")]
    MalformedHash,
}

/// Hash a plaintext password with argon2id + a fresh 16-byte random salt.
/// Returns a PHC-formatted string suitable for direct storage.
///
/// Blocking: ~60ms with the configured parameters. Wrap in `spawn_blocking`.
pub fn hash_password(plaintext: &str) -> Result<String, PasswordHashError> {
    use argon2::{Algorithm, Argon2, Params, Version};
    use password_hash::{PasswordHasher, SaltString, rand_core::OsRng};

    let params = Params::new(
        ARGON2_MEMORY_KIB,
        ARGON2_ITERATIONS,
        ARGON2_PARALLELISM,
        None,
    )
    .map_err(|_| PasswordHashError::InvalidParams)?;
    let hasher = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    let salt = SaltString::generate(&mut OsRng);
    hasher
        .hash_password(plaintext.as_bytes(), &salt)
        .map(|hash| hash.to_string())
        .map_err(|_| PasswordHashError::HashFailure)
}

/// Verify a plaintext password against a stored PHC-formatted hash.
/// Returns `Ok(true)` on match, `Ok(false)` on mismatch, `Err` if the stored
/// hash is malformed (indicates corruption, not user error).
///
/// Blocking: ~60ms. Wrap in `spawn_blocking`.
pub fn verify_password(plaintext: &str, stored_hash: &str) -> Result<bool, PasswordHashError> {
    use argon2::Argon2;
    use password_hash::{PasswordHash, PasswordVerifier};

    let parsed = PasswordHash::new(stored_hash).map_err(|_| PasswordHashError::MalformedHash)?;
    match Argon2::default().verify_password(plaintext.as_bytes(), &parsed) {
        Ok(()) => Ok(true),
        Err(password_hash::Error::Password) => Ok(false),
        Err(_) => Err(PasswordHashError::MalformedHash),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OriginPolicy {
    pub allow_any_origin: bool,
    pub is_production: bool,
    pub require_origin: bool,
    pub allowed_origins: BTreeSet<String>,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum SecurityError {
    #[error("invalid session code")]
    InvalidSessionCode,
    #[error("wildcard origins are not allowed in production")]
    WildcardOriginInProduction,
    #[error("invalid origin: {0}")]
    InvalidOrigin(String),
    #[error("production requires at least one allowed origin")]
    MissingProductionOrigin,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OriginPolicyOptions<'a> {
    pub allowed_origins: Option<&'a str>,
    pub app_origin: Option<&'a str>,
    pub is_production: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RateLimitDecision {
    pub allowed: bool,
    pub retry_after_ms: u64,
    pub remaining: u32,
}

#[derive(Debug, Default)]
pub struct FixedWindowRateLimiter {
    max_requests: u32,
    window_ms: u64,
    buckets: std::collections::BTreeMap<String, RateLimitBucket>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RateLimitBucket {
    count: u32,
    reset_at: u64,
}

pub fn normalize_origin(origin: &str) -> Option<String> {
    let url = Url::parse(origin).ok()?;
    Some(
        format!(
            "{}://{}{}",
            url.scheme(),
            url.host_str()?,
            url.port()
                .map(|port| format!(":{}", port))
                .unwrap_or_default()
        )
        .to_lowercase(),
    )
}

impl FixedWindowRateLimiter {
    pub fn new(max_requests: u32, window_ms: u64) -> Self {
        Self {
            max_requests,
            window_ms,
            buckets: std::collections::BTreeMap::new(),
        }
    }

    pub fn consume(&mut self, key: &str, now_ms: u64) -> RateLimitDecision {
        match self.buckets.get_mut(key) {
            Some(bucket) if bucket.reset_at > now_ms => {
                bucket.count += 1;
                RateLimitDecision {
                    allowed: bucket.count <= self.max_requests,
                    retry_after_ms: bucket.reset_at.saturating_sub(now_ms),
                    remaining: self.max_requests.saturating_sub(bucket.count),
                }
            }
            _ => {
                self.buckets.insert(
                    key.to_string(),
                    RateLimitBucket {
                        count: 1,
                        reset_at: now_ms + self.window_ms,
                    },
                );
                self.prune(now_ms);
                RateLimitDecision {
                    allowed: true,
                    retry_after_ms: 0,
                    remaining: self.max_requests.saturating_sub(1),
                }
            }
        }
    }

    fn prune(&mut self, now_ms: u64) {
        if self.buckets.len() <= 500 {
            return;
        }
        self.buckets.retain(|_, bucket| bucket.reset_at > now_ms);
    }
}

pub fn create_origin_policy(
    options: OriginPolicyOptions<'_>,
) -> Result<OriginPolicy, SecurityError> {
    let configured_origins = options
        .allowed_origins
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();

    let allow_any_origin = configured_origins.contains(&"*");
    if allow_any_origin && options.is_production {
        return Err(SecurityError::WildcardOriginInProduction);
    }

    let mut allowed_origins = BTreeSet::new();
    for origin in configured_origins {
        if origin == "*" {
            continue;
        }
        let normalized = normalize_origin(origin)
            .ok_or_else(|| SecurityError::InvalidOrigin(origin.to_string()))?;
        allowed_origins.insert(normalized);
    }

    if let Some(app_origin) = options.app_origin {
        let normalized = normalize_origin(app_origin)
            .ok_or_else(|| SecurityError::InvalidOrigin(app_origin.to_string()))?;
        allowed_origins.insert(normalized);
    }

    if options.is_production && allowed_origins.is_empty() {
        return Err(SecurityError::MissingProductionOrigin);
    }

    Ok(OriginPolicy {
        allow_any_origin,
        is_production: options.is_production,
        require_origin: options.is_production,
        allowed_origins,
    })
}

pub fn is_origin_allowed(origin: Option<&str>, policy: &OriginPolicy) -> bool {
    let Some(origin) = origin else {
        return !policy.require_origin;
    };

    let Some(normalized_origin) = normalize_origin(origin) else {
        return false;
    };

    if policy.allow_any_origin {
        return true;
    }

    if policy.allowed_origins.contains(&normalized_origin) {
        return true;
    }

    let Ok(url) = Url::parse(&normalized_origin) else {
        return false;
    };
    if !policy.is_production {
        return matches!(
            url.host_str(),
            Some("localhost") | Some("127.0.0.1") | Some("[::1]")
        );
    }

    false
}

pub fn validate_session_code(input: &str) -> Result<(), SecurityError> {
    if input.len() == SESSION_CODE_LENGTH && input.chars().all(|ch| ch.is_ascii_digit()) {
        Ok(())
    } else {
        Err(SecurityError::InvalidSessionCode)
    }
}

pub fn is_rust_session_code(code: &str, prefix: &str) -> bool {
    validate_session_code(code).is_ok() && code.starts_with(prefix)
}

pub fn estimate_data_url_bytes(data_url: &str) -> usize {
    let Some((_, base64_part)) = data_url.split_once(",") else {
        return usize::MAX;
    };
    let padding = base64_part
        .chars()
        .rev()
        .take_while(|ch| *ch == '=')
        .count();
    ((base64_part.len() * 3) / 4).saturating_sub(padding)
}

pub fn is_valid_png_data_url(data_url: &str, max_bytes: usize) -> bool {
    let Some(base64_part) = data_url.strip_prefix("data:image/png;base64,") else {
        return false;
    };

    let Ok(decoded) = base64::engine::general_purpose::STANDARD.decode(base64_part) else {
        return false;
    };

    if decoded.len() < PNG_SIGNATURE.len() || decoded[..PNG_SIGNATURE.len()] != PNG_SIGNATURE {
        return false;
    }

    let estimated = estimate_data_url_bytes(data_url);
    estimated > 0 && estimated <= max_bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tiny_png_data_url() -> String {
        let bytes = vec![
            0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x00,
        ];
        format!(
            "data:image/png;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(bytes)
        )
    }

    #[test]
    fn validate_session_code_accepts_six_digits() {
        assert_eq!(validate_session_code("123456"), Ok(()));
    }

    #[test]
    fn validate_session_code_rejects_non_digits() {
        assert_eq!(
            validate_session_code("12ab56"),
            Err(SecurityError::InvalidSessionCode)
        );
    }

    #[test]
    fn create_origin_policy_rejects_empty_production_allowlist() {
        let result = create_origin_policy(OriginPolicyOptions {
            allowed_origins: None,
            app_origin: None,
            is_production: true,
        });

        assert_eq!(result, Err(SecurityError::MissingProductionOrigin));
    }

    #[test]
    fn create_origin_policy_accepts_explicit_origin_list() {
        let policy = create_origin_policy(OriginPolicyOptions {
            allowed_origins: Some("https://example.com, https://game.example.com"),
            app_origin: None,
            is_production: true,
        })
        .expect("create origin policy");

        assert!(policy.allowed_origins.contains("https://example.com"));
        assert!(policy.allowed_origins.contains("https://game.example.com"));
    }

    #[test]
    fn loopback_origin_is_allowed_in_development() {
        let policy = create_origin_policy(OriginPolicyOptions {
            allowed_origins: None,
            app_origin: None,
            is_production: false,
        })
        .expect("create origin policy");

        assert!(is_origin_allowed(Some("http://localhost:5173"), &policy));
    }

    #[test]
    fn unknown_origin_is_rejected_in_production() {
        let policy = create_origin_policy(OriginPolicyOptions {
            allowed_origins: Some("https://example.com"),
            app_origin: None,
            is_production: true,
        })
        .expect("create origin policy");

        assert!(!is_origin_allowed(
            Some("https://evil.example.com"),
            &policy
        ));
    }

    #[test]
    fn png_data_url_validation_rejects_non_png_prefix() {
        let data_url = "data:image/jpeg;base64,Zm9v";
        assert!(!is_valid_png_data_url(data_url, 100));
    }

    #[test]
    fn png_data_url_validation_accepts_valid_small_png() {
        let data_url = tiny_png_data_url();
        assert!(is_valid_png_data_url(&data_url, 64));
    }

    #[test]
    fn png_data_url_validation_rejects_payload_over_limit() {
        let data_url = tiny_png_data_url();
        assert!(!is_valid_png_data_url(&data_url, 4));
    }

    #[test]
    fn rust_session_code_uses_prefix_and_numeric_validation() {
        assert!(is_rust_session_code(
            "912345",
            DEFAULT_RUST_SESSION_CODE_PREFIX
        ));
        assert!(!is_rust_session_code(
            "812345",
            DEFAULT_RUST_SESSION_CODE_PREFIX
        ));
        assert!(!is_rust_session_code(
            "9abc45",
            DEFAULT_RUST_SESSION_CODE_PREFIX
        ));
    }

    #[test]
    fn rate_limiter_blocks_after_limit_is_hit() {
        let mut limiter = FixedWindowRateLimiter::new(2, 1_000);

        let first = limiter.consume("ip:1", 0);
        let second = limiter.consume("ip:1", 100);
        let third = limiter.consume("ip:1", 200);

        assert!(first.allowed);
        assert!(second.allowed);
        assert!(!third.allowed);
        assert_eq!(third.remaining, 0);
    }

    #[test]
    fn rate_limiter_reports_retry_after_until_window_resets() {
        let mut limiter = FixedWindowRateLimiter::new(1, 1_000);
        limiter.consume("ip:1", 0);

        let blocked = limiter.consume("ip:1", 250);

        assert!(!blocked.allowed);
        assert_eq!(blocked.retry_after_ms, 750);
    }

    #[test]
    fn rate_limiter_resets_after_window_expires() {
        let mut limiter = FixedWindowRateLimiter::new(1, 1_000);
        limiter.consume("ip:1", 0);
        let blocked = limiter.consume("ip:1", 100);
        let reopened = limiter.consume("ip:1", 1_001);

        assert!(!blocked.allowed);
        assert!(reopened.allowed);
        assert_eq!(reopened.retry_after_ms, 0);
        assert_eq!(reopened.remaining, 0);
    }

    #[test]
    fn password_hash_round_trips() {
        let hash = hash_password("correct horse battery staple").expect("hash");
        assert!(hash.starts_with("$argon2id$"));
        assert!(verify_password("correct horse battery staple", &hash).unwrap());
        assert!(!verify_password("wrong password", &hash).unwrap());
    }

    #[test]
    fn password_hash_is_salted_per_call() {
        let a = hash_password("same-password").expect("hash a");
        let b = hash_password("same-password").expect("hash b");
        assert_ne!(a, b, "salt must differ per call");
    }

    #[test]
    fn verify_password_rejects_malformed_hash() {
        assert_eq!(
            verify_password("pw", "not-a-phc-string"),
            Err(PasswordHashError::MalformedHash)
        );
    }
}
