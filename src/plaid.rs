use crate::{
    db,
    domain::{self, Record},
    web::{AppState, random_token},
};
use aes_gcm::{
    Aes256Gcm, KeyInit, Nonce,
    aead::{Aead, Payload},
};
use anyhow::{Result, bail};
use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use rand::RngCore;
use serde_json::{Value, json};
use sqlx::PgConnection;

pub fn encrypt(key: &[u8; 32], plaintext: &str, aad: &str) -> Result<String> {
    let cipher =
        Aes256Gcm::new_from_slice(key).map_err(|_| anyhow::anyhow!("Invalid encryption key"))?;
    let mut nonce = [0u8; 12];
    rand::rng().fill_bytes(&mut nonce);
    let encrypted = cipher
        .encrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: plaintext.as_bytes(),
                aad: aad.as_bytes(),
            },
        )
        .map_err(|_| anyhow::anyhow!("Token encryption failed"))?;
    let mut bytes = nonce.to_vec();
    bytes.extend(encrypted);
    Ok(format!("worth:v1:{}", STANDARD.encode(bytes)))
}
pub fn decrypt(key: &[u8; 32], ciphertext: &str, aad: &str) -> Result<String> {
    let encoded = ciphertext
        .strip_prefix("worth:v1:")
        .ok_or_else(|| anyhow::anyhow!("Bank token needs migration; use bin/import-rails"))?;
    let bytes = STANDARD.decode(encoded)?;
    if bytes.len() < 28 {
        bail!("Invalid encrypted bank token")
    };
    let cipher =
        Aes256Gcm::new_from_slice(key).map_err(|_| anyhow::anyhow!("Invalid encryption key"))?;
    let value = cipher
        .decrypt(
            Nonce::from_slice(&bytes[..12]),
            Payload {
                msg: &bytes[12..],
                aad: aad.as_bytes(),
            },
        )
        .map_err(|_| anyhow::anyhow!("Unable to decrypt bank token; check WORTH_ENCRYPTION_KEY"))?;
    Ok(String::from_utf8(value)?)
}
fn aad(user: i64, item: &str) -> String {
    format!("worth:plaid:{user}:{item}")
}
async fn post(state: &AppState, path: &str, mut body: Value) -> Result<Value> {
    body["client_id"] = json!(
        std::env::var("PLAID_CLIENT_ID")
            .map_err(|_| anyhow::anyhow!("Configure PLAID_CLIENT_ID and PLAID_SECRET in .env"))?
    );
    body["secret"] = json!(
        std::env::var("PLAID_SECRET")
            .map_err(|_| anyhow::anyhow!("Configure PLAID_CLIENT_ID and PLAID_SECRET in .env"))?
    );
    let env = std::env::var("PLAID_ENV").unwrap_or_else(|_| "sandbox".into());
    let base = match env.as_str() {
        "sandbox" => "https://sandbox.plaid.com",
        "development" => "https://development.plaid.com",
        "production" => "https://production.plaid.com",
        _ => bail!("Unsupported Plaid environment"),
    };
    let response = state
        .http
        .post(format!("{base}{path}"))
        .header("Plaid-Version", "2020-09-14")
        .json(&body)
        .send()
        .await
        .map_err(|_| anyhow::anyhow!("Bank connection request failed; try again later"))?;
    let status = response.status();
    let result: Value = response
        .json()
        .await
        .map_err(|_| anyhow::anyhow!("Plaid returned an unreadable response"))?;
    if !status.is_success() || !result.s("error_code").is_empty() {
        bail!(
            "Plaid error: {}",
            if result.s("error_code").is_empty() {
                "provider_unavailable"
            } else {
                result.s("error_code")
            }
        )
    }
    Ok(result)
}
pub async fn enqueue(state: &AppState, user: i64, item: i64) -> Result<bool> {
    let mut tx = state.pool.begin().await?;
    let found: Option<i64> =
        sqlx::query_scalar("SELECT id FROM plaid_items WHERE user_id=$1 AND id=$2 FOR UPDATE")
            .bind(user)
            .bind(item)
            .fetch_optional(&mut *tx)
            .await?;
    if found.is_none() {
        bail!("Connection not found")
    };
    let token = random_token();
    let inserted=sqlx::query("INSERT INTO sync_jobs(plaid_item_id,request_token) VALUES($1,$2) ON CONFLICT(plaid_item_id) DO NOTHING").bind(item).bind(&token).execute(&mut *tx).await?.rows_affected()>0;
    if inserted {
        sqlx::query(
            "UPDATE plaid_items SET sync_requested_at=now(),sync_request_token=$1 WHERE id=$2",
        )
        .bind(token)
        .bind(item)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(inserted)
}
pub async fn api(state: &AppState, user: i64, path: &str, p: &Value) -> Result<Response> {
    if path.ends_with("/link_token") {
        let products = std::env::var("PLAID_PRODUCTS").unwrap_or_else(|_| "transactions".into());
        let countries = std::env::var("PLAID_COUNTRY_CODES").unwrap_or_else(|_| "US".into());
        let mut body = json!({"client_name":"Worth","user":{"client_user_id":user.to_string()},"products":products.split(',').map(str::trim).collect::<Vec<_>>(),"country_codes":countries.split(',').map(str::trim).collect::<Vec<_>>(),"language":"en","transactions":{"days_requested":std::env::var("PLAID_TRANSACTIONS_DAYS_REQUESTED").ok().and_then(|s|s.parse::<u32>().ok()).unwrap_or(730)}});
        if let Ok(uri) = std::env::var("PLAID_REDIRECT_URI") {
            body["redirect_uri"] = json!(uri)
        }
        let response = post(state, "/link/token/create", body).await?;
        return Ok(Json(json!({"link_token":response["link_token"]})).into_response());
    }
    if path.ends_with("/exchange_public_token") {
        if p.s("public_token").is_empty() || p.s("institution_id").is_empty() {
            bail!("Public token and institution are required")
        }
        let exchange = post(
            state,
            "/item/public_token/exchange",
            json!({"public_token":p["public_token"]}),
        )
        .await?;
        let token = exchange.s("access_token");
        let provider = exchange.s("item_id");
        if token.is_empty() || provider.is_empty() {
            bail!("Plaid returned an incomplete connection")
        }
        let accounts = post(state, "/accounts/get", json!({"access_token":token})).await?;
        let mut tx = state.pool.begin().await?;
        sqlx::query("SELECT id FROM users WHERE id=$1 FOR UPDATE")
            .bind(user)
            .execute(&mut *tx)
            .await?;
        let existing: Option<Value> = sqlx::query_scalar(
            "SELECT to_jsonb(p) FROM plaid_items p WHERE plaid_item_id=$1 FOR UPDATE",
        )
        .bind(provider)
        .fetch_optional(&mut *tx)
        .await?;
        if existing.as_ref().is_some_and(|r| r.id("user_id") != user) {
            bail!("This bank connection is already linked to another user")
        }
        let institution:i64=sqlx::query_scalar("INSERT INTO institutions(plaid_institution_id,name) VALUES($1,$2) ON CONFLICT(plaid_institution_id) DO UPDATE SET name=excluded.name,updated_at=now() RETURNING id").bind(p.s("institution_id")).bind(if p.s("institution_name").is_empty(){p.s("institution_id")}else{p.s("institution_name")}).fetch_one(&mut *tx).await?;
        let item=db::save(&mut tx,"plaid_items",user,existing.map(|r|r.id("id")),json!({"institution_id":institution,"plaid_item_id":provider,"access_token":encrypt(&state.encryption_key,token,&aad(user,provider))?,"status":"active"})).await?;
        let rows = accounts["accounts"]
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("Plaid account list missing"))?;
        upsert_accounts(&mut tx, user, item.id("id"), rows).await?;
        tx.commit().await?;
        enqueue(state, user, item.id("id")).await?;
        return Ok(
            Json(json!({"plaid_item":{"id":item.id("id")},"accounts":rows.len()})).into_response(),
        );
    }
    if path.ends_with("/sync") {
        let id = path.split('/').nth(5).unwrap_or("").parse::<i64>()?;
        let enqueued = enqueue(state, user, id).await?;
        if p["async"] == false || p.s("async") == "false" || p.s("async") == "0" {
            if !enqueued {
                return Ok((
                    StatusCode::CONFLICT,
                    Json(json!({"error":"sync_in_progress"})),
                )
                    .into_response());
            }
            let job:Value=sqlx::query_scalar("UPDATE sync_jobs SET locked_at=now(),attempts=attempts+1 WHERE plaid_item_id=$1 AND locked_at IS NULL RETURNING to_jsonb(sync_jobs)").bind(id).fetch_optional(&state.pool).await?.ok_or_else(||anyhow::anyhow!("Refresh already started in background"))?;
            let result = run_job(state, &job).await;
            finish_job(state, &job, &result).await?;
            return Ok(Json(result?).into_response());
        }
        return Ok((StatusCode::ACCEPTED,Json(json!({"enqueued":enqueued,"status":if enqueued{"enqueued"}else{"busy"},"plaid_item":{"id":id}}))).into_response());
    }
    Ok((StatusCode::NOT_FOUND, Json(json!({"error":"not_found"}))).into_response())
}
pub async fn upsert_accounts(
    conn: &mut PgConnection,
    user: i64,
    item: i64,
    rows: &[Value],
) -> Result<()> {
    for payload in rows {
        let id: Option<i64> =
            sqlx::query_scalar("SELECT id FROM accounts WHERE user_id=$1 AND plaid_account_id=$2")
                .bind(user)
                .bind(payload.s("account_id"))
                .fetch_optional(&mut *conn)
                .await?;
        let b = &payload["balances"];
        let currency = if b.s("iso_currency_code").is_empty() {
            b["unofficial_currency_code"].clone()
        } else {
            b["iso_currency_code"].clone()
        };
        let account=db::save(conn,"accounts",user,id,json!({"plaid_item_id":item,"plaid_account_id":payload["account_id"],"name":payload["name"],"official_name":payload["official_name"],"account_type":payload["type"],"subtype":payload["subtype"],"mask":payload["mask"],"iso_currency_code":currency,"current_balance":b["current"],"available_balance":b["available"],"limit_balance":b["limit"]})).await?;
        if !b["current"].is_null() {
            let existing:Option<i64>=sqlx::query_scalar("SELECT id FROM balance_snapshots WHERE user_id=$1 AND account_id=$2 AND as_of_date=$3").bind(user).bind(account.id("id")).bind(domain::today()).fetch_optional(&mut *conn).await?;
            db::save(conn,"balance_snapshots",user,existing,json!({"account_id":account.id("id"),"as_of_date":domain::today(),"current_balance":b["current"],"available_balance":b["available"],"iso_currency_code":currency})).await?;
        }
    }
    Ok(())
}
pub async fn upsert_transactions(
    conn: &mut PgConnection,
    user: i64,
    item: i64,
    rows: &[Value],
) -> Result<()> {
    let rules = db::rows(conn, "transaction_rules", user).await?;
    for p in rows {
        let account: i64 = sqlx::query_scalar(
            "SELECT id FROM accounts WHERE user_id=$1 AND plaid_item_id=$2 AND plaid_account_id=$3",
        )
        .bind(user)
        .bind(item)
        .bind(p.s("account_id"))
        .fetch_optional(&mut *conn)
        .await?
        .ok_or_else(|| anyhow::anyhow!("Bank transaction references an unavailable account"))?;
        let existing:Option<Value>=sqlx::query_scalar("SELECT to_jsonb(t) FROM transactions t WHERE user_id=$1 AND (plaid_transaction_id=$2 OR ($3<>'' AND plaid_transaction_id=$3)) ORDER BY (plaid_transaction_id=$2) DESC LIMIT 1").bind(user).bind(p.s("transaction_id")).bind(p.s("pending_transaction_id")).fetch_optional(&mut *conn).await?;
        let id = existing.as_ref().map(|t| t.id("id"));
        let mut t = existing.clone().unwrap_or_else(|| json!({"user_id":user}));
        let changed_split = t.flag("is_split") && t.dec("amount") != p.dec("amount");
        let currency = if !p.s("iso_currency_code").is_empty() {
            p["iso_currency_code"].clone()
        } else {
            p["unofficial_currency_code"].clone()
        };
        let attrs = json!({"account_id":account,"plaid_item_id":item,"plaid_transaction_id":p["transaction_id"],"source":"plaid","source_transaction_id":null,"pending":p.flag("pending"),"amount":p["amount"],"iso_currency_code":currency,"date":p["date"],"authorized_date":p["authorized_date"],"name":if !p.s("name").is_empty(){p.s("name")}else if !p.s("merchant_name").is_empty(){p.s("merchant_name")}else{"Transaction"},"merchant_name":p["merchant_name"],"merchant_entity_id":p["merchant_entity_id"],"payment_channel":p["payment_channel"],"pending_transaction_id":p["pending_transaction_id"],"original_description":p["original_description"],"raw_payload":p});
        for (k, v) in attrs.as_object().unwrap() {
            t[k] = v.clone()
        }
        if changed_split {
            t["split_needs_review"] = json!(true)
        }
        if id.is_none() {
            for r in &rules {
                domain::apply_rule(r, &mut t)
            }
        }
        let t = db::save(conn, "transactions", user, id, t).await?;
        if t.flag("is_split") {
            sqlx::query("UPDATE transactions SET date=$1,iso_currency_code=$2,updated_at=now() WHERE user_id=$3 AND parent_id=$4").bind(t.day("date")).bind(t.s("iso_currency_code")).bind(user).bind(t.id("id")).execute(&mut *conn).await?;
        }
    }
    Ok(())
}
async fn run_job(state: &AppState, job: &Value) -> Result<Value> {
    let item: Value = sqlx::query_scalar(
        "SELECT to_jsonb(p) FROM plaid_items p WHERE id=$1 AND sync_request_token=$2",
    )
    .bind(job.id("plaid_item_id"))
    .bind(job.s("request_token"))
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| anyhow::anyhow!("Refresh request superseded"))?;
    let user = item.id("user_id");
    let token = decrypt(
        &state.encryption_key,
        item.s("access_token"),
        &aad(user, item.s("plaid_item_id")),
    )?;
    let accounts = post(state, "/accounts/get", json!({"access_token":token})).await?;
    let mut attempt = 0;
    let (added, modified, removed, cursor) = loop {
        attempt += 1;
        let mut cursor = item.s("cursor").to_string();
        let mut added = vec![];
        let mut modified = vec![];
        let mut removed = vec![];
        let mut retry = false;
        loop {
            let mut body = json!({"access_token":token,"count":500});
            if !cursor.is_empty() {
                body["cursor"] = json!(cursor)
            }
            let response = match post(state, "/transactions/sync", body).await {
                Ok(v) => v,
                Err(e)
                    if e.to_string()
                        .contains("TRANSACTIONS_SYNC_MUTATION_DURING_PAGINATION")
                        && attempt < 2 =>
                {
                    retry = true;
                    break;
                }
                Err(e) => return Err(e),
            };
            for (key, dest) in [
                ("added", &mut added),
                ("modified", &mut modified),
                ("removed", &mut removed),
            ] {
                dest.extend(
                    response[key]
                        .as_array()
                        .ok_or_else(|| anyhow::anyhow!("Incomplete sync page"))?
                        .iter()
                        .cloned(),
                )
            }
            cursor = response.s("next_cursor").into();
            if cursor.is_empty() {
                bail!("Plaid sync cursor missing")
            };
            if !response.flag("has_more") {
                break;
            }
        }
        if retry {
            continue;
        }
        break (added, modified, removed, cursor);
    };
    let mut tx = state.pool.begin().await?;
    sqlx::query("SELECT id FROM users WHERE id=$1 FOR UPDATE")
        .bind(user)
        .execute(&mut *tx)
        .await?;
    let current = db::owned(&mut tx, "plaid_items", user, item.id("id")).await?;
    if current.s("sync_request_token") != job.s("request_token")
        || current.s("cursor") != item.s("cursor")
    {
        bail!("Refresh request superseded")
    };
    upsert_accounts(
        &mut tx,
        user,
        item.id("id"),
        accounts["accounts"]
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("Bank accounts missing"))?,
    )
    .await?;
    upsert_transactions(&mut tx, user, item.id("id"), &added).await?;
    upsert_transactions(&mut tx, user, item.id("id"), &modified).await?;
    for r in &removed {
        let id:Option<i64>=sqlx::query_scalar("SELECT id FROM transactions WHERE user_id=$1 AND plaid_item_id=$2 AND plaid_transaction_id=$3").bind(user).bind(item.id("id")).bind(r.s("transaction_id")).fetch_optional(&mut *tx).await?;
        if let Some(id) = id {
            sqlx::query("DELETE FROM transactions WHERE user_id=$1 AND parent_id=$2")
                .bind(user)
                .bind(id)
                .execute(&mut *tx)
                .await?;
            db::delete(&mut tx, "transactions", user, id).await?;
        }
    }
    domain::refresh(&mut tx, user).await?;
    db::save(
        &mut tx,
        "plaid_items",
        user,
        Some(item.id("id")),
        json!({"cursor":cursor,"last_synced_at":chrono::Utc::now().naive_utc(),"status":"active"}),
    )
    .await?;
    tx.commit().await?;
    Ok(
        json!({"plaid_item":{"id":item.id("id")},"added":added.len(),"modified":modified.len(),"removed":removed.len()}),
    )
}
async fn finish_job(state: &AppState, job: &Value, result: &Result<Value>) -> Result<()> {
    let mut tx = state.pool.begin().await?;
    if result.is_ok() || job.id("attempts") >= 5 {
        sqlx::query("DELETE FROM sync_jobs WHERE id=$1 AND request_token=$2")
            .bind(job.id("id"))
            .bind(job.s("request_token"))
            .execute(&mut *tx)
            .await?;
        sqlx::query("UPDATE plaid_items SET sync_requested_at=NULL,sync_request_token=NULL,status=CASE WHEN $1 THEN status ELSE 'errored' END WHERE id=$2 AND sync_request_token=$3").bind(result.is_ok()).bind(job.id("plaid_item_id")).bind(job.s("request_token")).execute(&mut *tx).await?;
    } else {
        sqlx::query("UPDATE sync_jobs SET locked_at=NULL,available_at=now()+interval '1 minute'*attempts,last_error='Refresh failed; retry scheduled' WHERE id=$1 AND request_token=$2").bind(job.id("id")).bind(job.s("request_token")).execute(&mut *tx).await?;
        sqlx::query(
            "UPDATE plaid_items SET status='errored' WHERE id=$1 AND sync_request_token=$2",
        )
        .bind(job.id("plaid_item_id"))
        .bind(job.s("request_token"))
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}
pub async fn worker(state: AppState) {
    loop {
        let job=sqlx::query_scalar::<_,Value>("UPDATE sync_jobs SET locked_at=now(),attempts=attempts+1 WHERE id=(SELECT id FROM sync_jobs WHERE available_at<=now() AND (locked_at IS NULL OR locked_at<now()-interval '20 minutes') ORDER BY id FOR UPDATE SKIP LOCKED LIMIT 1) RETURNING to_jsonb(sync_jobs)").fetch_optional(&state.pool).await;
        match job {
            Ok(Some(job)) => {
                let result = run_job(&state, &job).await;
                if result.is_err() {
                    tracing::warn!(
                        job_id = job.id("id"),
                        "Bank refresh failed; see connection status"
                    )
                };
                if finish_job(&state, &job, &result).await.is_err() {
                    tracing::error!("Unable to update bank refresh job")
                }
            }
            Ok(None) => {
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
            Err(_) => {
                tracing::error!("Bank refresh queue unavailable");
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            }
        }
    }
}
