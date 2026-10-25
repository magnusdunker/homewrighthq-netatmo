//! OAuth 2.0 and the Netatmo Connect API.
//!
//! The user creates an app on dev.netatmo.com and logs in with the
//! authorization code flow. The access token lasts three hours; Netatmo
//! hands out a new refresh token on every refresh, so the new one must be
//! saved. Netatmo answers an expired or invalid access token with HTTP 403
//! and error code 2 or 3 in the body, and a used up request quota with
//! error code 26 (or HTTP 429).

use std::time::{Duration, Instant};

use rand::distributions::Alphanumeric;
use rand::Rng;
use serde_json::Value;
use tokio::sync::Mutex;

pub const AUTHORIZE_URL: &str = "https://api.netatmo.com/oauth2/authorize";
pub const TOKEN_URL: &str = "https://api.netatmo.com/oauth2/token";
pub const API_URL: &str = "https://api.netatmo.com";
/// Weather station, thermostats and valves, and indoor and outdoor cameras.
pub const SCOPE: &str = "read_station read_thermostat write_thermostat read_camera write_camera read_presence write_presence";

#[derive(Clone, Debug, PartialEq)]
pub struct Urls {
    pub authorize: String,
    pub token: String,
    pub api: String,
}

impl Default for Urls {
    fn default() -> Self {
        Self { authorize: AUTHORIZE_URL.into(), token: TOKEN_URL.into(), api: API_URL.into() }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ApiError {
    /// The refresh token or the code was refused; the user must log in again.
    Auth(String),
    /// Netatmo's quota is used up (error 26 or HTTP 429).
    RateLimited,
    /// Netatmo refused a command (HTTP 400 or a 403 that is not about the token).
    Refused(String),
    Other(String),
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ApiError::Auth(e) => write!(f, "Netatmo afviste login: {e}"),
            ApiError::RateLimited => write!(f, "Netatmos grænse for forespørgsler er nået"),
            ApiError::Refused(e) => write!(f, "Netatmo afviste kommandoen: {e}"),
            ApiError::Other(e) => write!(f, "{e}"),
        }
    }
}

impl From<reqwest::Error> for ApiError {
    fn from(e: reqwest::Error) -> Self {
        ApiError::Other(e.to_string())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Tokens {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_in: u64,
}

pub fn http() -> reqwest::Client {
    reqwest::Client::builder().timeout(Duration::from_secs(30)).build().unwrap_or_default()
}

/// A random `state` for the authorization request.
pub fn new_state() -> String {
    rand::thread_rng().sample_iter(&Alphanumeric).take(32).map(char::from).collect()
}

/// The address the user opens to log in at Netatmo.
pub fn authorize_url(urls: &Urls, client_id: &str, redirect_uri: &str, state: &str) -> String {
    let mut u = reqwest::Url::parse(&urls.authorize).expect("authorize url");
    u.query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", client_id)
        .append_pair("redirect_uri", redirect_uri)
        .append_pair("scope", SCOPE)
        .append_pair("state", state);
    u.to_string()
}

/// The code from what the user pasted: the whole address Netatmo sent the
/// browser to, or only the code. `Err` is a form error code.
pub fn parse_redirect(input: &str, state: &str) -> Result<String, &'static str> {
    let s = input.trim();
    if s.is_empty() {
        return Err("missing_code");
    }
    let Ok(u) = reqwest::Url::parse(s) else {
        // not an address: take it as the code itself
        return if s.contains(char::is_whitespace) { Err("missing_code") } else { Ok(s.to_string()) };
    };
    let get = |k: &str| u.query_pairs().find(|(n, _)| n == k).map(|(_, v)| v.to_string());
    if get("error").is_some() {
        return Err("access_denied");
    }
    if get("state").is_some_and(|st| st != state) {
        return Err("state_mismatch");
    }
    get("code").filter(|c| !c.is_empty()).ok_or("missing_code")
}

async fn token_request(http: &reqwest::Client, urls: &Urls, form: &[(&str, &str)]) -> Result<Tokens, ApiError> {
    let res = http.post(&urls.token).form(form).send().await?;
    let status = res.status();
    let v: Value = res.json().await.unwrap_or(Value::Null);
    if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
        return Err(ApiError::RateLimited);
    }
    if status.is_client_error() {
        let e = v["error"].as_str().unwrap_or("").to_string();
        let d = v["error_description"].as_str().unwrap_or("").to_string();
        return Err(ApiError::Auth(format!("HTTP {status} {e} {d}").trim().to_string()));
    }
    if !status.is_success() {
        return Err(ApiError::Other(format!("token: HTTP {status}")));
    }
    let access_token = v["access_token"].as_str().filter(|t| !t.is_empty()).ok_or_else(|| ApiError::Other("intet access_token".into()))?.to_string();
    Ok(Tokens { access_token, refresh_token: v["refresh_token"].as_str().map(String::from), expires_in: v["expires_in"].as_u64().unwrap_or(10800) })
}

pub async fn exchange_code(http: &reqwest::Client, urls: &Urls, client_id: &str, secret: &str, redirect_uri: &str, code: &str) -> Result<Tokens, ApiError> {
    token_request(
        http,
        urls,
        &[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", redirect_uri),
            ("scope", SCOPE),
            ("client_id", client_id),
            ("client_secret", secret),
        ],
    )
    .await
}

pub async fn refresh(http: &reqwest::Client, urls: &Urls, client_id: &str, secret: &str, refresh_token: &str) -> Result<Tokens, ApiError> {
    token_request(http, urls, &[("grant_type", "refresh_token"), ("refresh_token", refresh_token), ("client_id", client_id), ("client_secret", secret)]).await
}

struct TokenState {
    access: Option<(String, Instant)>,
    refresh: String,
}

/// A request body.
pub enum Body<'a> {
    None,
    Form(&'a [(&'a str, String)]),
    Json(&'a Value),
}

/// The Netatmo API with tokens that refresh themselves.
pub struct Client {
    http: reqwest::Client,
    urls: Urls,
    client_id: String,
    secret: String,
    tokens: Mutex<TokenState>,
    /// A refresh token that replaced the stored one and must be saved.
    new_refresh: std::sync::Mutex<Option<String>>,
}

/// What a non-success answer from the API means. Netatmo puts a code in
/// `{"error": {"code": n, "message": ...}}`.
fn classify(status: reqwest::StatusCode, body: &str) -> (Option<i64>, String) {
    let v: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    let code = v["error"]["code"].as_i64();
    let msg = v["error"]["message"].as_str().map(String::from).unwrap_or_else(|| body.chars().take(200).collect());
    (code, format!("HTTP {status}: {msg}"))
}

impl Client {
    pub fn new(urls: Urls, client_id: &str, secret: &str, refresh_token: &str, access: Option<Tokens>) -> Self {
        let access = access.map(|t| (t.access_token, Instant::now() + Duration::from_secs(t.expires_in)));
        Self {
            http: http(),
            urls,
            client_id: client_id.into(),
            secret: secret.into(),
            tokens: Mutex::new(TokenState { access, refresh: refresh_token.into() }),
            new_refresh: Default::default(),
        }
    }

    async fn token(&self, force: bool) -> Result<String, ApiError> {
        let mut t = self.tokens.lock().await;
        if !force {
            if let Some((a, until)) = &t.access {
                if Instant::now() + Duration::from_secs(60) < *until {
                    return Ok(a.clone());
                }
            }
        }
        let new = refresh(&self.http, &self.urls, &self.client_id, &self.secret, &t.refresh).await?;
        if let Some(r) = new.refresh_token.clone().filter(|r| *r != t.refresh) {
            t.refresh = r.clone();
            *self.new_refresh.lock().unwrap() = Some(r);
        }
        t.access = Some((new.access_token.clone(), Instant::now() + Duration::from_secs(new.expires_in)));
        Ok(new.access_token)
    }

    /// The refresh token that must be saved, once.
    pub fn take_new_refresh(&self) -> Option<String> {
        self.new_refresh.lock().unwrap().take()
    }

    async fn send(&self, method: reqwest::Method, path: &str, body: Body<'_>) -> Result<Value, ApiError> {
        for attempt in 0..2 {
            let token = self.token(attempt > 0).await?;
            let mut req = self.http.request(method.clone(), format!("{}{path}", self.urls.api)).bearer_auth(token);
            req = match &body {
                Body::None => req,
                Body::Form(f) => req.form(f),
                Body::Json(j) => req.json(j),
            };
            let res = req.send().await?;
            let status = res.status();
            let text = res.text().await.unwrap_or_default();
            if status.is_success() {
                if text.trim().is_empty() {
                    return Ok(Value::Null);
                }
                return serde_json::from_str(&text).map_err(|e| ApiError::Other(format!("ugyldigt svar: {e}")));
            }
            if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                return Err(ApiError::RateLimited);
            }
            let (code, msg) = classify(status, &text);
            // invalid or expired access token: refresh once
            if matches!(code, Some(2 | 3)) || status == reqwest::StatusCode::UNAUTHORIZED {
                if attempt == 0 {
                    continue;
                }
                return Err(ApiError::Auth(msg));
            }
            if code == Some(26) {
                return Err(ApiError::RateLimited);
            }
            if matches!(status.as_u16(), 400 | 403 | 404) {
                return Err(ApiError::Refused(msg));
            }
            return Err(ApiError::Other(msg));
        }
        Err(ApiError::Auth("HTTP 403".into()))
    }

    /// Weather stations with their modules and latest measurements.
    pub async fn stations(&self) -> Result<Value, ApiError> {
        self.send(reqwest::Method::GET, "/api/getstationsdata", Body::None).await
    }

    /// Homes with their rooms, modules and known persons.
    pub async fn homes(&self) -> Result<Value, ApiError> {
        self.send(reqwest::Method::GET, "/api/homesdata", Body::None).await
    }

    /// The current state of one home's rooms and modules.
    pub async fn home_status(&self, home_id: &str) -> Result<Value, ApiError> {
        let path = format!("/api/homestatus?home_id={}", encode(home_id));
        self.send(reqwest::Method::GET, &path, Body::None).await
    }

    /// Set one room: `mode` is `manual` (with `temp`), `max`, `off` or `home`.
    pub async fn set_room(&self, home_id: &str, room_id: &str, mode: &str, temp: Option<f64>) -> Result<(), ApiError> {
        let mut form = vec![("home_id", home_id.to_string()), ("room_id", room_id.to_string()), ("mode", mode.to_string())];
        if let Some(t) = temp {
            form.push(("temp", t.to_string()));
        }
        self.send(reqwest::Method::POST, "/api/setroomthermpoint", Body::Form(&form)).await.map(|_| ())
    }

    /// Set the whole home: `schedule`, `away` or `hg` (frost guard).
    pub async fn set_home_mode(&self, home_id: &str, mode: &str) -> Result<(), ApiError> {
        let form = [("home_id", home_id.to_string()), ("mode", mode.to_string())];
        self.send(reqwest::Method::POST, "/api/setthermmode", Body::Form(&form)).await.map(|_| ())
    }

    /// Change fields of modules, such as a camera's `monitoring`.
    pub async fn set_state(&self, body: &Value) -> Result<(), ApiError> {
        self.send(reqwest::Method::POST, "/api/setstate", Body::Json(body)).await.map(|_| ())
    }
}

/// Percent-encodes a query value. Netatmo ids hold `:` and similar.
fn encode(s: &str) -> String {
    let mut u = reqwest::Url::parse("http://x/").expect("url");
    u.query_pairs_mut().append_pair("v", s);
    u.query().unwrap_or("v=").trim_start_matches("v=").to_string()
}
