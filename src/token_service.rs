//! Loopback-only token service for local LIVE metadata development.
//!
//! The SoundCloud client secret is read only by this process. The UI receives a
//! short-lived access-token lease over a loopback connection protected by a
//! separate local service key. Nothing is written to disk or logged.

#[cfg(test)]
use std::collections::VecDeque;
use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::Engine;
use serde::{Deserialize, Serialize};
use url::form_urlencoded;

const SOUND_CLOUD_TOKEN_URL: &str = "https://secure.soundcloud.com/oauth/token";
const DEFAULT_BIND: &str = "127.0.0.1:8787";
const TOKEN_SKEW: Duration = Duration::from_secs(60);
const TOKEN_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_REQUEST_BYTES: usize = 8 * 1024;

#[derive(Clone)]
pub struct TokenServiceConfig {
    client_id: String,
    client_secret: String,
    local_service_key: String,
    bind: SocketAddr,
}

impl TokenServiceConfig {
    pub fn from_environment() -> Result<Self, TokenServiceError> {
        let client_id = required_env("SOUNDCLOUD_CLIENT_ID")?;
        let client_secret = required_env("SOUNDCLOUD_CLIENT_SECRET")?;
        let local_service_key = required_env("SOUNDCLOUD_TOKEN_SERVICE_KEY")?;
        let bind = std::env::var("SOUNDCLOUD_TOKEN_SERVICE_BIND")
            .unwrap_or_else(|_| DEFAULT_BIND.to_owned())
            .parse::<SocketAddr>()
            .map_err(|_| TokenServiceError::Configuration("SOUNDCLOUD_TOKEN_SERVICE_BIND"))?;
        if bind.ip() != IpAddr::V4(Ipv4Addr::LOCALHOST) {
            return Err(TokenServiceError::Configuration(
                "token service must bind exactly to 127.0.0.1",
            ));
        }
        Ok(Self {
            client_id,
            client_secret,
            local_service_key,
            bind,
        })
    }

    #[cfg(test)]
    fn fixture() -> Self {
        Self {
            client_id: "fixture-client".to_owned(),
            client_secret: "fixture-secret".to_owned(),
            local_service_key: "fixture-loopback-key".to_owned(),
            bind: DEFAULT_BIND.parse().unwrap(),
        }
    }
}

fn required_env(name: &'static str) -> Result<String, TokenServiceError> {
    let value = std::env::var(name).map_err(|_| TokenServiceError::Configuration(name))?;
    if value.trim().is_empty() {
        return Err(TokenServiceError::Configuration(name));
    }
    Ok(value)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TokenServiceError {
    Configuration(&'static str),
    Unauthorized,
    Forbidden,
    RateLimited,
    Timeout,
    Network,
    InvalidResponse,
    Unavailable,
}

impl TokenServiceError {
    pub const fn status_code(&self) -> u16 {
        match self {
            Self::Configuration(_) | Self::Unavailable => 503,
            Self::Unauthorized => 401,
            Self::Forbidden => 403,
            Self::RateLimited => 429,
            Self::Timeout => 504,
            Self::Network | Self::InvalidResponse => 502,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum TokenGrant {
    ClientCredentials,
    Refresh { refresh_token: String },
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct IssuedToken {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: Duration,
}

trait TokenIssuer: Send + Sync + 'static {
    fn issue(
        &self,
        config: &TokenServiceConfig,
        grant: TokenGrant,
    ) -> Result<IssuedToken, TokenServiceError>;
}

struct UreqTokenIssuer {
    agent: ureq::Agent,
}

impl Default for UreqTokenIssuer {
    fn default() -> Self {
        Self {
            agent: ureq::AgentBuilder::new()
                .timeout(TOKEN_TIMEOUT)
                .redirects(0)
                .build(),
        }
    }
}

impl TokenIssuer for UreqTokenIssuer {
    fn issue(
        &self,
        config: &TokenServiceConfig,
        grant: TokenGrant,
    ) -> Result<IssuedToken, TokenServiceError> {
        let (form, basic_auth) = match grant {
            TokenGrant::ClientCredentials => (
                form_urlencoded::Serializer::new(String::new())
                    .append_pair("grant_type", "client_credentials")
                    .finish(),
                Some(
                    base64::engine::general_purpose::STANDARD
                        .encode(format!("{}:{}", config.client_id, config.client_secret)),
                ),
            ),
            TokenGrant::Refresh { refresh_token } => (
                form_urlencoded::Serializer::new(String::new())
                    .append_pair("grant_type", "refresh_token")
                    .append_pair("client_id", &config.client_id)
                    .append_pair("client_secret", &config.client_secret)
                    .append_pair("refresh_token", &refresh_token)
                    .finish(),
                None,
            ),
        };
        let mut request = self
            .agent
            .post(SOUND_CLOUD_TOKEN_URL)
            .set("Accept", "application/json; charset=utf-8")
            .set("Content-Type", "application/x-www-form-urlencoded");
        if let Some(encoded) = basic_auth {
            request = request.set("Authorization", &format!("Basic {encoded}"));
        }
        let response = match request.send_string(&form) {
            Ok(response) => response,
            Err(ureq::Error::Status(_, response)) => {
                return Err(token_error_from_status(response.status()));
            }
            Err(ureq::Error::Transport(error)) => {
                return Err(
                    if error.to_string().to_ascii_lowercase().contains("timeout") {
                        TokenServiceError::Timeout
                    } else {
                        TokenServiceError::Network
                    },
                );
            }
        };
        let mut bytes = Vec::new();
        response
            .into_reader()
            .take(64 * 1024)
            .read_to_end(&mut bytes)
            .map_err(|_| TokenServiceError::Network)?;
        let wire: TokenWire =
            serde_json::from_slice(&bytes).map_err(|_| TokenServiceError::InvalidResponse)?;
        let access_token = wire
            .access_token
            .filter(|value| !value.trim().is_empty())
            .ok_or(TokenServiceError::InvalidResponse)?;
        let expires_in = wire
            .expires_in
            .filter(|seconds| *seconds > 0)
            .map(Duration::from_secs)
            .ok_or(TokenServiceError::InvalidResponse)?;
        Ok(IssuedToken {
            access_token,
            refresh_token: wire.refresh_token.filter(|value| !value.trim().is_empty()),
            expires_in,
        })
    }
}

fn token_error_from_status(status: u16) -> TokenServiceError {
    match status {
        401 => TokenServiceError::Unauthorized,
        403 => TokenServiceError::Forbidden,
        429 => TokenServiceError::RateLimited,
        408 | 504 => TokenServiceError::Timeout,
        _ => TokenServiceError::Network,
    }
}

#[derive(Deserialize)]
struct TokenWire {
    access_token: Option<String>,
    refresh_token: Option<String>,
    expires_in: Option<u64>,
}

#[derive(Clone)]
struct CachedToken {
    access_token: String,
    refresh_token: Option<String>,
    expires_at: Instant,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LocalTokenLease {
    pub access_token: String,
    pub expires_at_unix: u64,
}

/// Memory-only cache. Its lock remains held during an upstream renewal so at
/// most one request can consume a single-use refresh token.
pub struct TokenService {
    config: TokenServiceConfig,
    issuer: Arc<dyn TokenIssuer>,
    cached: Mutex<Option<CachedToken>>,
}

impl TokenService {
    pub fn from_environment() -> Result<Self, TokenServiceError> {
        let config = TokenServiceConfig::from_environment()?;
        Ok(Self::new(config, Arc::new(UreqTokenIssuer::default())))
    }

    fn new(config: TokenServiceConfig, issuer: Arc<dyn TokenIssuer>) -> Self {
        Self {
            config,
            issuer,
            cached: Mutex::new(None),
        }
    }

    pub fn lease(&self) -> Result<LocalTokenLease, TokenServiceError> {
        let mut cache = self
            .cached
            .lock()
            .map_err(|_| TokenServiceError::Unavailable)?;
        if let Some(cached) = cache.as_ref().filter(|token| token_is_valid(token)) {
            return Ok(lease_from_cached(cached));
        }

        let renewed = match cache
            .as_ref()
            .and_then(|cached| cached.refresh_token.clone())
        {
            Some(refresh_token) => self
                .issuer
                .issue(&self.config, TokenGrant::Refresh { refresh_token })
                .or_else(|_| {
                    self.issuer
                        .issue(&self.config, TokenGrant::ClientCredentials)
                })?,
            None => self
                .issuer
                .issue(&self.config, TokenGrant::ClientCredentials)?,
        };
        let cached_token = CachedToken {
            access_token: renewed.access_token,
            refresh_token: renewed.refresh_token,
            expires_at: Instant::now() + renewed.expires_in,
        };
        let lease = lease_from_cached(&cached_token);
        *cache = Some(cached_token);
        Ok(lease)
    }

    fn local_service_key(&self) -> &str {
        &self.config.local_service_key
    }

    fn bind(&self) -> SocketAddr {
        self.config.bind
    }
}

fn token_is_valid(token: &CachedToken) -> bool {
    Instant::now()
        .checked_add(TOKEN_SKEW)
        .is_some_and(|deadline| deadline < token.expires_at)
}

fn lease_from_cached(token: &CachedToken) -> LocalTokenLease {
    let remaining = token.expires_at.saturating_duration_since(Instant::now());
    let expires_at_unix = SystemTime::now()
        .checked_add(remaining)
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    LocalTokenLease {
        access_token: token.access_token.clone(),
        expires_at_unix,
    }
}

pub fn run_from_environment() -> Result<(), TokenServiceError> {
    let service = Arc::new(TokenService::from_environment()?);
    let listener = TcpListener::bind(service.bind()).map_err(|_| TokenServiceError::Unavailable)?;
    println!(
        "SoundCloud local token service listening on {}",
        service.bind()
    );
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => handle_connection(stream, &service),
            Err(_) => continue,
        }
    }
    Ok(())
}

fn handle_connection(mut stream: TcpStream, service: &TokenService) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(3)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(3)));
    let mut request = vec![0_u8; MAX_REQUEST_BYTES];
    let Ok(read) = stream.read(&mut request) else {
        return;
    };
    let request = String::from_utf8_lossy(&request[..read]);
    let authorized = request_header(&request, "x-soundcloud-local-key").is_some_and(|value| {
        constant_time_eq(value.as_bytes(), service.local_service_key().as_bytes())
    });
    let first_line = request.lines().next().unwrap_or_default();
    if first_line != "GET /v1/token HTTP/1.1" && first_line != "GET /v1/token HTTP/1.0" {
        let _ = write_json(&mut stream, 404, br#"{"error":"not_found"}"#);
        return;
    }
    if !authorized {
        let _ = write_json(&mut stream, 401, br#"{"error":"unauthorized"}"#);
        return;
    }
    match service.lease() {
        Ok(lease) => match serde_json::to_vec(&lease) {
            Ok(json) => {
                let _ = write_json(&mut stream, 200, &json);
            }
            Err(_) => {
                let _ = write_json(&mut stream, 502, br#"{"error":"invalid_response"}"#);
            }
        },
        Err(error) => {
            let body = format!(r#"{{"error":"token_service_{}"}}"#, error.status_code());
            let _ = write_json(&mut stream, error.status_code(), body.as_bytes());
        }
    }
}

fn request_header<'a>(request: &'a str, expected_name: &str) -> Option<&'a str> {
    request.lines().skip(1).find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.eq_ignore_ascii_case(expected_name)
            .then_some(value.trim())
    })
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    let longest = left.len().max(right.len());
    let mut difference = left.len() ^ right.len();
    for index in 0..longest {
        difference |= (left.get(index).copied().unwrap_or(0)
            ^ right.get(index).copied().unwrap_or(0)) as usize;
    }
    difference == 0
}

fn write_json(stream: &mut TcpStream, status: u16, body: &[u8]) -> std::io::Result<()> {
    let reason = match status {
        200 => "OK",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        429 => "Too Many Requests",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        504 => "Gateway Timeout",
        _ => "Error",
    };
    write!(
        stream,
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nCache-Control: no-store\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )?;
    stream.write_all(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FixtureIssuer {
        requests: Mutex<Vec<TokenGrant>>,
        responses: Mutex<VecDeque<Result<IssuedToken, TokenServiceError>>>,
    }

    impl FixtureIssuer {
        fn new(responses: Vec<Result<IssuedToken, TokenServiceError>>) -> Self {
            Self {
                requests: Mutex::new(Vec::new()),
                responses: Mutex::new(responses.into()),
            }
        }
    }

    impl TokenIssuer for FixtureIssuer {
        fn issue(
            &self,
            _: &TokenServiceConfig,
            grant: TokenGrant,
        ) -> Result<IssuedToken, TokenServiceError> {
            self.requests.lock().unwrap().push(grant);
            self.responses
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or(Err(TokenServiceError::Network))
        }
    }

    fn token(access_token: &str, refresh_token: Option<&str>, expires_in: u64) -> IssuedToken {
        IssuedToken {
            access_token: access_token.to_owned(),
            refresh_token: refresh_token.map(str::to_owned),
            expires_in: Duration::from_secs(expires_in),
        }
    }

    #[test]
    fn token_cache_reuses_a_valid_lease_without_a_second_upstream_request() {
        let issuer = Arc::new(FixtureIssuer::new(vec![Ok(token(
            "one",
            Some("refresh"),
            3600,
        ))]));
        let service = TokenService::new(TokenServiceConfig::fixture(), issuer.clone());
        assert_eq!(service.lease().unwrap().access_token, "one");
        assert_eq!(service.lease().unwrap().access_token, "one");
        assert_eq!(
            issuer.requests.lock().unwrap().as_slice(),
            [TokenGrant::ClientCredentials]
        );
    }

    #[test]
    fn expired_token_refreshes_once_and_rotates_the_cached_refresh_token() {
        let issuer = Arc::new(FixtureIssuer::new(vec![
            Ok(token("expired", Some("refresh-one"), 0)),
            Ok(token("fresh", Some("refresh-two"), 3600)),
        ]));
        let service = TokenService::new(TokenServiceConfig::fixture(), issuer.clone());
        assert_eq!(service.lease().unwrap().access_token, "expired");
        assert_eq!(service.lease().unwrap().access_token, "fresh");
        assert_eq!(
            issuer.requests.lock().unwrap().as_slice(),
            [
                TokenGrant::ClientCredentials,
                TokenGrant::Refresh {
                    refresh_token: "refresh-one".to_owned()
                }
            ]
        );
        assert_eq!(service.lease().unwrap().access_token, "fresh");
        assert_eq!(issuer.requests.lock().unwrap().len(), 2);
    }

    #[test]
    fn failed_single_use_refresh_falls_back_once_to_client_credentials() {
        let issuer = Arc::new(FixtureIssuer::new(vec![
            Ok(token("expired", Some("refresh-one"), 0)),
            Err(TokenServiceError::Unauthorized),
            Ok(token("replacement", Some("refresh-two"), 3600)),
        ]));
        let service = TokenService::new(TokenServiceConfig::fixture(), issuer.clone());
        let _ = service.lease().unwrap();
        assert_eq!(service.lease().unwrap().access_token, "replacement");
        assert_eq!(issuer.requests.lock().unwrap().len(), 3);
    }

    #[test]
    fn request_headers_and_errors_never_include_client_secret() {
        let error = TokenServiceError::Configuration("SOUNDCLOUD_CLIENT_SECRET");
        assert!(!format!("{error:?}").contains("fixture-secret"));
        assert!(constant_time_eq(b"same", b"same"));
        assert!(!constant_time_eq(b"same", b"different"));
        assert_eq!(
            request_header("GET / HTTP/1.1\r\nX-Key: value\r\n", "x-key"),
            Some("value")
        );
    }
}
