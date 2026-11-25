use crate::{
    db,
    domain::{self, Record},
};
use anyhow::{Result, bail};
use axum::{
    Router,
    body::to_bytes,
    extract::{DefaultBodyLimit, FromRequest, Multipart, Request, State},
    http::{StatusCode, header},
    response::{Html, IntoResponse, Redirect, Response},
    routing::get,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use minijinja::Environment;
use serde_json::{Value, json};
use sqlx::{PgPool, postgres::PgPoolOptions};
use std::{collections::HashMap, sync::Arc};
use subtle::ConstantTimeEq;

#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    pub templates: Arc<Environment<'static>>,
    pub encryption_key: [u8; 32],
    pub secure_cookies: bool,
    pub http: reqwest::Client,
    pub asset_root: std::path::PathBuf,
}
impl AppState {
    pub async fn from_env() -> Result<Self> {
        let url = std::env::var("DATABASE_URL")
            .unwrap_or_else(|_| "postgresql://localhost/worth_rust_development".into());
        let options: url::Url = url.parse()?;
        let name = options.path().trim_start_matches('/');
        if !name.starts_with("worth_rust_") {
            bail!(
                "DATABASE_URL must use a separate worth_rust_* database. Never point Rust at the Rails database."
            )
        }
        let pool = PgPoolOptions::new()
            .max_connections(10)
            .connect(&url)
            .await?;
        db::initialize(&pool).await?;
        let key = STANDARD.decode(std::env::var("WORTH_ENCRYPTION_KEY").map_err(|_| {
            anyhow::anyhow!(
                "Set WORTH_ENCRYPTION_KEY to a base64-encoded 32-byte key; see bin/setup"
            )
        })?)?;
        let encryption_key: [u8; 32] = key
            .try_into()
            .map_err(|_| anyhow::anyhow!("WORTH_ENCRYPTION_KEY must decode to 32 bytes"))?;
        Self::new(
            pool,
            encryption_key,
            std::env::var("COOKIE_SECURE").as_deref() == Ok("true"),
        )
    }
    pub fn new(pool: PgPool, encryption_key: [u8; 32], secure_cookies: bool) -> Result<Self> {
        let mut env = Environment::new();
        let asset_root = std::env::var_os("WORTH_ASSET_ROOT")
            .map(std::path::PathBuf::from)
            .unwrap_or(std::env::current_dir()?);
        env.set_loader(minijinja::path_loader(asset_root.join("templates")));
        env.add_filter("money", |v: minijinja::Value, currency: Option<String>| {
            let currency = currency.as_deref().unwrap_or("USD");
            if v.is_none() || v.is_undefined() {
                return "Unavailable".to_string();
            }
            let n = v
                .to_string()
                .parse::<rust_decimal::Decimal>()
                .unwrap_or_default()
                .round_dp(2);
            let text = format!("{:.2}", n.abs());
            let (whole, fraction) = text.split_once('.').unwrap_or((&text, "00"));
            let mut grouped = String::new();
            for (i, c) in whole.chars().enumerate() {
                if i > 0 && (whole.len() - i) % 3 == 0 {
                    grouped.push(',')
                }
                grouped.push(c)
            }
            format!(
                "{}{}{}.{}",
                if n.is_sign_negative() { "−" } else { "" },
                if currency == "USD" {
                    "$".into()
                } else {
                    format!("{currency} ")
                },
                grouped,
                fraction
            )
        });
        env.add_filter("urlencode", |s: String| {
            url::form_urlencoded::byte_serialize(s.as_bytes()).collect::<String>()
        });
        Ok(Self {
            pool,
            templates: Arc::new(env),
            asset_root,
            encryption_key,
            secure_cookies,
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(40))
                .build()?,
        })
    }
}
pub fn router(state: AppState) -> Router {
    Router::new().route("/up",get(||async{"OK"})).nest_service("/assets",tower_http::services::ServeDir::new(state.asset_root.join("static")))
    .route("/icon.png",get(||async{([(header::CONTENT_TYPE,"image/png")],include_bytes!("../static/icon.png").as_slice())}))
    .route("/icon.svg",get(||async{([(header::CONTENT_TYPE,"image/svg+xml")],include_str!("../static/icon.svg"))}))
    .route("/manifest.json",get(||async{axum::Json(json!({"name":"Worth","short_name":"Worth","start_url":"/","display":"standalone","theme_color":"#008d82","background_color":"#f4f7f6","icons":[{"src":"/icon.png","sizes":"512x512","type":"image/png"}]}))}))
    .route("/service-worker",get(||async{([(header::CONTENT_TYPE,"text/javascript")],"// Worth intentionally does not cache private financial pages.\n")}))
    .fallback(dispatch).layer(DefaultBodyLimit::max(26*1024*1024)).layer(tower_http::trace::TraceLayer::new_for_http()).with_state(state)
}
pub struct Session {
    pub token: String,
    pub csrf: String,
    pub user: Option<Value>,
}
pub struct Input {
    pub params: Value,
    pub uploads: HashMap<String, (String, Vec<u8>)>,
}
fn insert_param(value: &mut Value, parts: &[&str], text: Value) {
    if parts.is_empty() {
        *value = text;
        return;
    }
    if parts[0].is_empty() {
        if !value.is_array() {
            *value = json!([])
        }
        value.as_array_mut().unwrap().push(text);
        return;
    }
    if !value.is_object() {
        *value = json!({})
    }
    let next = value
        .as_object_mut()
        .unwrap()
        .entry(parts[0])
        .or_insert(Value::Null);
    insert_param(next, &parts[1..], text)
}
pub fn parse_pairs(raw: &str) -> Value {
    let mut value = json!({});
    for (k, v) in url::form_urlencoded::parse(raw.as_bytes()) {
        let normalized = k.replace(']', "");
        let parts = normalized.split('[').collect::<Vec<_>>();
        insert_param(&mut value, &parts, json!(v))
    }
    value
}
async fn input(req: Request) -> Result<Input> {
    let query = req.uri().query().unwrap_or("").to_string();
    let mut params = parse_pairs(&query);
    let mut uploads = HashMap::new();
    let content = req
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    if content.starts_with("multipart/form-data") {
        let mut form = Multipart::from_request(req, &())
            .await
            .map_err(|_| anyhow::anyhow!("Invalid upload"))?;
        while let Some(field) = form.next_field().await? {
            let name = field.name().unwrap_or("").to_string();
            let filename = field.file_name().map(str::to_owned);
            let bytes = field.bytes().await?;
            if let Some(filename) = filename {
                if !bytes.is_empty() {
                    uploads.insert(name, (filename, bytes.to_vec()));
                }
            } else {
                let text = String::from_utf8(bytes.to_vec())?;
                let normalized = name.replace(']', "");
                insert_param(
                    &mut params,
                    &normalized.split('[').collect::<Vec<_>>(),
                    json!(text),
                );
            }
        }
    } else {
        let bytes = to_bytes(req.into_body(), 26 * 1024 * 1024).await?;
        if !bytes.is_empty() {
            let body = if content.starts_with("application/json") {
                serde_json::from_slice::<Value>(&bytes)?
            } else {
                parse_pairs(std::str::from_utf8(&bytes)?)
            };
            if let Some(map) = body.as_object() {
                for (k, v) in map {
                    params[k] = v.clone()
                }
            }
        }
    }
    Ok(Input { params, uploads })
}
async fn session(state: &AppState, cookie: &str) -> Result<Session> {
    let token = cookie
        .split(';')
        .filter_map(|s| s.trim().split_once('='))
        .find(|(k, _)| *k == "_worth_rust_session")
        .map(|(_, v)| v)
        .unwrap_or("");
    if token.len() == 64 {
        let row: Option<Value> = sqlx::query_scalar(
            "SELECT to_jsonb(s) FROM web_sessions s WHERE token_hash=$1 AND expires_at>now()",
        )
        .bind(domain::hash(token))
        .fetch_optional(&state.pool)
        .await?;
        if let Some(row) = row {
            let user: Option<Value> = if row.id("user_id") != 0 {
                sqlx::query_scalar(
                    "SELECT to_jsonb(u) FROM users u WHERE id=$1 AND session_token=$2",
                )
                .bind(row.id("user_id"))
                .bind(row.s("user_session_token"))
                .fetch_optional(&state.pool)
                .await?
            } else {
                None
            };
            if row.id("user_id") == 0 || user.is_some() {
                return Ok(Session {
                    token: token.into(),
                    csrf: row.s("csrf_token").into(),
                    user,
                });
            }
        }
    }
    let token = random_token();
    let csrf = random_token();
    sqlx::query("INSERT INTO web_sessions(token_hash,csrf_token) VALUES($1,$2)")
        .bind(domain::hash(&token))
        .bind(&csrf)
        .execute(&state.pool)
        .await?;
    Ok(Session {
        token,
        csrf,
        user: None,
    })
}
pub fn random_token() -> String {
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}
async fn rotate(state: &AppState, session: &mut Session, user: Option<Value>) -> Result<()> {
    let token = random_token();
    let csrf = random_token();
    let mut tx = state.pool.begin().await?;
    sqlx::query("DELETE FROM web_sessions WHERE token_hash=$1")
        .bind(domain::hash(&session.token))
        .execute(&mut *tx)
        .await?;
    sqlx::query("INSERT INTO web_sessions(token_hash,csrf_token,user_id,user_session_token) VALUES($1,$2,$3,$4)").bind(domain::hash(&token)).bind(&csrf).bind(user.as_ref().map(|u|u.id("id"))).bind(user.as_ref().map(|u|u.s("session_token"))).execute(&mut *tx).await?;
    tx.commit().await?;
    session.token = token;
    session.csrf = csrf;
    session.user = user;
    Ok(())
}
async fn dispatch(State(state): State<AppState>, req: Request) -> Response {
    let method = req.method().clone();
    let path = req.uri().path().trim_end_matches('/').to_string();
    let path = if path.is_empty() {
        "/".to_string()
    } else {
        path
    };
    let cookie = req
        .headers()
        .get(header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let header_csrf = req
        .headers()
        .get("x-csrf-token")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let origin = req
        .headers()
        .get(header::ORIGIN)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let host = req
        .headers()
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let mut session = match session(&state, &cookie).await {
        Ok(s) => s,
        Err(_) => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                "Worth is temporarily unavailable",
            )
                .into_response();
        }
    };
    let input = match input(req).await {
        Ok(v) => v,
        Err(_) => return (StatusCode::BAD_REQUEST, "Invalid request or upload").into_response(),
    };
    let is_get = method == axum::http::Method::GET || method == axum::http::Method::HEAD;
    let effective_method = if input.params.s("_method").is_empty() {
        method.as_str().to_string()
    } else {
        input.params.s("_method").to_uppercase()
    };
    let recovery_params = input.params.clone();
    let result: Result<Response> = async {
        if !is_get {
            let supplied = if !header_csrf.is_empty() {
                header_csrf.as_str()
            } else {
                input.params.s("authenticity_token")
            };
            if !bool::from(supplied.as_bytes().ct_eq(session.csrf.as_bytes())) {
                return Ok((
                    StatusCode::FORBIDDEN,
                    "Your form expired. Reload the page and try again.",
                )
                    .into_response());
            }
            if let Some(origin) = origin {
                let u = url::Url::parse(&origin)?;
                let authority = match u.port() {
                    Some(port) => format!("{}:{port}", u.host_str().unwrap_or("")),
                    None => u.host_str().unwrap_or("").to_string(),
                };
                if authority != host {
                    return Ok(
                        (StatusCode::FORBIDDEN, "Request origin does not match Worth")
                            .into_response(),
                    );
                }
            }
        }
        if ["/sign_in", "/sign_up", "/sign_out"].contains(&path.as_str()) && !is_get {
            return authenticate(&state, &mut session, &path, &input.params).await;
        }
        if session.user.is_none() && !(["/sign_in", "/sign_up"].contains(&path.as_str()) && is_get)
        {
            if path.starts_with("/api/") {
                return Ok((
                    StatusCode::UNAUTHORIZED,
                    axum::Json(json!({"error":"unauthorized"})),
                )
                    .into_response());
            }
            return Ok(Redirect::to("/sign_in").into_response());
        }
        if is_get {
            crate::views::page(&state, &session, &path, &input.params, None).await
        } else {
            crate::actions::mutate(&state, &mut session, &path, &effective_method, input).await
        }
    }
    .await;
    let mut response = match result {
        Ok(r) => r,
        Err(error) => {
            let message = if let Some(db_error) = error.downcast_ref::<sqlx::Error>() {
                match db_error
                    .as_database_error()
                    .and_then(|e| e.code())
                    .as_deref()
                {
                    Some("23505") => {
                        "That record already exists. Choose a different value.".to_string()
                    }
                    _ => {
                        "The change could not be saved. Check the fields and try again.".to_string()
                    }
                }
            } else {
                error.to_string()
            };
            tracing::warn!(path=%path,"Request validation or operation failed");
            if path.starts_with("/api/") {
                (
                    StatusCode::UNPROCESSABLE_ENTITY,
                    axum::Json(json!({"error":message})),
                )
                    .into_response()
            } else {
                let body = format!(
                    "<!doctype html><html lang=\"en\"><head><title>Review your changes · Worth</title><link rel=\"stylesheet\" href=\"/assets/application.css\"><link rel=\"stylesheet\" href=\"/assets/worth.css\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"></head><body><main class=\"error-page\"><a class=\"wordmark\" href=\"/\">worth</a><h1>Review your changes</h1><p role=\"alert\">{}</p><p>Use Back to return to your form with your entries, then correct the fields and submit again.</p><button onclick=\"history.back()\">Back to form</button></main></body></html>",
                    escape(&message)
                );
                let mut recovery = recovery_params.clone();
                recovery["_recover_form"] = json!(!is_get);
                let recovery_path = path
                    .strip_suffix("/split")
                    .or_else(|| path.strip_suffix("/unsplit"))
                    .unwrap_or(&path);
                let recovery_path = if !is_get && recovery_path == "/transactions" {
                    "/transactions/new"
                } else {
                    recovery_path
                };
                match crate::views::page(&state, &session, recovery_path, &recovery, Some(&message))
                    .await
                {
                    Ok(mut response) => {
                        *response.status_mut() = StatusCode::UNPROCESSABLE_ENTITY;
                        response
                    }
                    Err(_) => (StatusCode::UNPROCESSABLE_ENTITY, Html(body)).into_response(),
                }
            }
        }
    };
    let cookie = format!(
        "_worth_rust_session={}; Path=/; HttpOnly; SameSite=Lax; Max-Age=1209600{}",
        session.token,
        if state.secure_cookies { "; Secure" } else { "" }
    );
    response
        .headers_mut()
        .insert(header::SET_COOKIE, cookie.parse().unwrap());
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    response
        .headers_mut()
        .insert("x-content-type-options", "nosniff".parse().unwrap());
    response
        .headers_mut()
        .insert("referrer-policy", "same-origin".parse().unwrap());
    response
        .headers_mut()
        .insert("x-frame-options", "DENY".parse().unwrap());
    response
}
pub fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}
pub fn redirect(path: &str, note: &str) -> Response {
    let separator = if path.contains('?') { '&' } else { '?' };
    let query = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("notice", note)
        .finish();
    Redirect::to(&format!("{path}{separator}{query}")).into_response()
}
async fn authenticate(
    state: &AppState,
    session: &mut Session,
    path: &str,
    p: &Value,
) -> Result<Response> {
    if path == "/sign_out" {
        rotate(state, session, None).await?;
        return Ok(Redirect::to("/sign_in").into_response());
    }
    let fields = if p["user"].is_object() { &p["user"] } else { p };
    let email = fields.s("email").trim().to_lowercase();
    let password = fields.s("password").to_string();
    let user = if path == "/sign_up" {
        if !email.contains('@') || email.len() > 254 {
            bail!("Enter a valid email address")
        }
        if password.len() < 8 || password.len() > 72 {
            bail!("Password must be between 8 and 72 bytes")
        }
        if password != fields.s("password_confirmation") {
            bail!("Passwords do not match")
        }
        let digest =
            tokio::task::spawn_blocking(move || bcrypt::hash(password, bcrypt::DEFAULT_COST))
                .await??;
        sqlx::query_scalar::<_,Value>("INSERT INTO users(email,password_digest,session_token) VALUES($1,$2,$3) RETURNING to_jsonb(users)").bind(email).bind(digest).bind(random_token()).fetch_one(&state.pool).await?
    } else {
        let user: Option<Value> =
            sqlx::query_scalar("SELECT to_jsonb(u) FROM users u WHERE lower(email)=$1")
                .bind(&email)
                .fetch_optional(&state.pool)
                .await?;
        let digest = user
            .as_ref()
            .map(|u| u.s("password_digest").to_string())
            .unwrap_or_else(|| {
                "$2b$12$Nn8wXhJMhu5W3nfQLKBBjuOvFUUKjEi9hCJYJ/c4qsp9X3VRmOlP2".into()
            });
        let valid =
            tokio::task::spawn_blocking(move || bcrypt::verify(password, &digest).unwrap_or(false))
                .await?;
        if !valid || user.is_none() {
            bail!("Email or password is incorrect")
        }
        user.unwrap()
    };
    rotate(state, session, Some(user)).await?;
    Ok(Redirect::to("/").into_response())
}
pub async fn change_password(
    state: &AppState,
    session: &mut Session,
    p: &Value,
) -> Result<Response> {
    let user = session.user.as_ref().unwrap();
    let fields = if p["user"].is_object() { &p["user"] } else { p };
    let current = fields.s("current_password").to_string();
    let digest = user.s("password_digest").to_string();
    if !tokio::task::spawn_blocking(move || bcrypt::verify(current, &digest).unwrap_or(false))
        .await?
    {
        bail!("Current password is incorrect")
    }
    let password = fields.s("password").to_string();
    if password.len() < 8 || password.len() > 72 || password != fields.s("password_confirmation") {
        bail!("Use matching passwords between 8 and 72 bytes")
    }
    let digest =
        tokio::task::spawn_blocking(move || bcrypt::hash(password, bcrypt::DEFAULT_COST)).await??;
    let user=sqlx::query_scalar("UPDATE users SET password_digest=$1,session_token=$2,updated_at=now() WHERE id=$3 RETURNING to_jsonb(users)").bind(digest).bind(random_token()).bind(user.id("id")).fetch_one(&state.pool).await?;
    rotate(state, session, Some(user)).await?;
    Ok(redirect(
        "/",
        "Password changed; other sessions have been signed out.",
    ))
}
