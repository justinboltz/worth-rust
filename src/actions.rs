use crate::{
    db,
    domain::{self, Ledger, Record},
    imports, plaid,
    web::{self, AppState, Input, Session},
};
use anyhow::{Result, bail};
use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use chrono::Months;
use serde_json::{Value, json};
use sqlx::PgConnection;

pub fn fields(p: &Value, name: &str) -> Value {
    if p[name].is_object() {
        p[name].clone()
    } else {
        p.clone()
    }
}
pub fn select(p: &Value, keys: &[&str]) -> Value {
    let mut out = json!({});
    for k in keys {
        if !p[*k].is_null() {
            out[*k] = p[*k].clone()
        }
    }
    out
}
pub fn typed(mut attrs: Value) -> Result<Value> {
    for (k, v) in attrs.as_object_mut().unwrap() {
        if k.ends_with("_id") {
            *v = if v
                .as_str()
                .is_some_and(|s| s.is_empty() || s == "uncategorized")
            {
                Value::Null
            } else {
                json!(
                    v.as_i64()
                        .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
                        .ok_or_else(|| anyhow::anyhow!("Choose an available record"))?
                )
            };
        } else if [
            "hidden",
            "duplicate",
            "transfer",
            "card_payment",
            "rollover",
            "month_end",
            "tracked",
            "net_worth_excluded",
            "category_locked",
            "classification_override",
        ]
        .contains(&k.as_str())
        {
            *v = json!(
                v.as_bool()
                    .unwrap_or_else(|| matches!(v.as_str(), Some("1" | "true" | "on")))
            )
        } else if ["expense_type"].contains(&k.as_str()) && (v == "" || v == "automatic") {
            *v = Value::Null
        }
    }
    Ok(attrs)
}
fn valid_currency(s: &str) -> Result<()> {
    if !domain::CURRENCIES.contains(&s) {
        bail!("Choose a supported currency")
    }
    Ok(())
}
fn required(p: &Value, k: &str) -> Result<()> {
    if p.s(k).trim().is_empty() {
        bail!("{} is required", k.replace('_', " "))
    }
    Ok(())
}
fn merge(a: &mut Value, b: Value) {
    for (k, v) in b.as_object().unwrap() {
        a[k] = v.clone()
    }
}
fn owned_category(l: &Ledger, id: i64) -> Result<()> {
    if id != 0 && l.category(id).is_none() {
        bail!("Category is unavailable")
    }
    Ok(())
}
fn owned_account(l: &Ledger, id: i64) -> Result<()> {
    if id != 0 && l.account(id).is_none() {
        bail!("Account is unavailable")
    }
    Ok(())
}
fn validate_expense(v: &Value) -> Result<()> {
    if !v.s("expense_type").is_empty()
        && !["fixed", "variable", "unclassified"].contains(&v.s("expense_type"))
    {
        bail!("Choose a valid expense type")
    }
    Ok(())
}
pub async fn mutate(
    state: &AppState,
    session: &mut Session,
    path: &str,
    method: &str,
    input: Input,
) -> Result<Response> {
    let user = session.user.as_ref().unwrap().id("id");
    let p = &input.params;
    if path == "/password" {
        return web::change_password(state, session, p).await;
    }
    if path.starts_with("/api/v1/plaid/") {
        return plaid::api(state, user, path, p).await;
    }
    if path == "/imports" || path.starts_with("/imports/") {
        return imports::handle(state, user, path, input).await;
    }
    if path.starts_with("/plaid_items/")
        && (path.ends_with("/sync") || path.ends_with("/refresh_stale"))
    {
        let ids: Vec<i64> = if path.ends_with("/refresh_stale") {
            sqlx::query_scalar("SELECT id FROM plaid_items WHERE user_id=$1 AND (last_synced_at IS NULL OR last_synced_at<now()-interval '1 hour')").bind(user).fetch_all(&state.pool).await?
        } else {
            vec![path.split('/').nth(2).unwrap_or("").parse()?]
        };
        let mut count = 0;
        for id in ids {
            if plaid::enqueue(state, user, id).await? {
                count += 1
            }
        }
        return Ok(web::redirect(
            "/",
            &format!("Queued {count} bank connections for refresh."),
        ));
    }
    let parts = path.trim_matches('/').split('/').collect::<Vec<_>>();
    let section = parts[0];
    let id = parts.get(1).and_then(|s| s.parse::<i64>().ok());
    let action = parts.get(2).copied().unwrap_or("");
    let mut tx = state.pool.begin().await?;
    sqlx::query("SELECT id FROM users WHERE id=$1 FOR UPDATE")
        .bind(user)
        .execute(&mut *tx)
        .await?;
    let l = Ledger::load(&mut tx, user).await?;
    let response = match section {
        "transactions" => transactions(&mut tx, user, &l, id, action, path, p).await?,
        "categories" => categories(&mut tx, user, &l, id, action, p).await?,
        "transaction_rules" => rules(&mut tx, user, &l, id, method, p).await?,
        "budgets" => budgets(&mut tx, user, &l, id, method, p).await?,
        "my-assets" => assets(&mut tx, user, id, p).await?,
        "recurring" => recurring(&mut tx, user, &l, id, path, method, p).await?,
        "plaid_items" if action == "tracking" => {
            let item = id.ok_or_else(|| anyhow::anyhow!("Connection is required"))?;
            db::owned(&mut tx, "plaid_items", user, item).await?;
            let ids = p["account_ids"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .iter()
                .filter_map(|v| {
                    v.as_str()
                        .and_then(|s| s.parse::<i64>().ok())
                        .or_else(|| v.as_i64())
                })
                .collect::<Vec<_>>();
            if ids.iter().any(|id| {
                !l.accounts
                    .iter()
                    .any(|a| a.id("id") == *id && a.id("plaid_item_id") == item)
            }) {
                bail!("Choose accounts from this bank connection")
            };
            sqlx::query("UPDATE accounts SET tracked=(id=ANY($1)),updated_at=now() WHERE user_id=$2 AND plaid_item_id=$3").bind(&ids).bind(user).bind(item).execute(&mut *tx).await?;
            web::redirect("/", "Account tracking updated.")
        }
        _ => return Ok((StatusCode::NOT_FOUND, "Page not found").into_response()),
    };
    tx.commit().await?;
    Ok(response)
}
async fn transactions(
    conn: &mut PgConnection,
    user: i64,
    l: &Ledger,
    id: Option<i64>,
    action: &str,
    path: &str,
    p: &Value,
) -> Result<Response> {
    if path.ends_with("/classify") || path.ends_with("/categorize") {
        domain::refresh(conn, user).await?;
        return Ok(web::redirect(
            "/transactions",
            "Automatic detection and categorization completed.",
        ));
    }
    if path.ends_with("/bulk_update") {
        let ids = p["ids"]
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("Select at least one transaction"))?
            .iter()
            .filter_map(|v| {
                v.as_str()
                    .and_then(|s| s.parse::<i64>().ok())
                    .or_else(|| v.as_i64())
            })
            .collect::<HashSet<_>>();
        if ids.is_empty() {
            bail!("Select at least one transaction")
        }
        if ids.iter().any(|id| l.transaction(*id).is_none()) {
            bail!("Transaction is unavailable")
        }
        if p.s("duplicate_review") == "1" {
            let groups = duplicate_groups(l);
            if !groups
                .iter()
                .any(|g| ids.len() < g.len() && ids.iter().all(|id| g.contains(id)))
            {
                bail!("Select copies from one group and leave at least one transaction unselected")
            }
        }
        let bulk = if p.s("duplicate_review") == "1" {
            json!({"duplicate":"1"})
        } else {
            fields(p, "bulk")
        };
        let mut attrs = select(
            &bulk,
            &[
                "expense_type",
                "category_id",
                "tags",
                "hidden",
                "duplicate",
                "transfer",
                "card_payment",
            ],
        );
        attrs.as_object_mut().unwrap().retain(|_, v| v != "");
        if attrs.as_object().unwrap().is_empty() {
            bail!("Choose at least one change")
        };
        attrs = typed(attrs)?;
        validate_expense(&attrs)?;
        owned_category(l, attrs.id("category_id"))?;
        if attrs.get("category_id").is_some() {
            merge(
                &mut attrs,
                json!({"category_locked":true,"category_origin":"manual","category_reason":"Your bulk category edit","suggested_category_id":null,"category_suggestion_reason":null}),
            );
        }
        if attrs.get("transfer").is_some() || attrs.get("card_payment").is_some() {
            merge(
                &mut attrs,
                json!({"classification_override":true,"classification_origin":"manual","classification_reason":"Your bulk cash-flow classification","matched_transaction_id":null,"suggested_classification":null,"suggestion_reason":null}),
            );
        }
        for id in ids {
            db::save(conn, "transactions", user, Some(id), attrs.clone()).await?;
        }
        domain::refresh(conn, user).await?;
        return Ok(web::redirect(
            "/transactions",
            "Selected transactions updated.",
        ));
    }
    if action == "split" {
        let id = id.unwrap();
        let parent = db::owned(conn, "transactions", user, id).await?;
        if parent.id("parent_id") != 0 {
            bail!("A split cannot be split again")
        }
        let rows = p["splits"]
            .as_object()
            .ok_or_else(|| anyhow::anyhow!("Enter split rows"))?
            .values()
            .filter(|r| !r.s("name").is_empty() || !r.s("amount").is_empty())
            .collect::<Vec<_>>();
        if rows.len() < 2 {
            bail!("Enter at least two split rows")
        }
        let mut total = rust_decimal::Decimal::ZERO;
        for r in &rows {
            total += domain::amount(r.s("amount"))?;
            owned_category(l, r.id("category_id"))?
        }
        if total != parent.dec("amount") {
            bail!("Splits must add up to the original amount")
        }
        sqlx::query("DELETE FROM transactions WHERE user_id=$1 AND parent_id=$2")
            .bind(user)
            .bind(id)
            .execute(&mut *conn)
            .await?;
        for r in rows {
            let mut child = select(
                &parent,
                &[
                    "account_id",
                    "plaid_item_id",
                    "source",
                    "source_account_name",
                    "date",
                    "iso_currency_code",
                    "tags",
                ],
            );
            merge(
                &mut child,
                json!({"parent_id":id,"amount":r.s("amount"),"name":if r.s("name").is_empty(){parent.s("name")}else{r.s("name")},"category_id":if r.id("category_id")==0{Value::Null}else{json!(r.id("category_id"))},"category_locked":true,"category_origin":"split","category_reason":"Your split category"}),
            );
            db::save(conn, "transactions", user, None, child).await?;
        }
        db::save(
            conn,
            "transactions",
            user,
            Some(id),
            json!({"is_split":true,"split_needs_review":false}),
        )
        .await?;
        return Ok(web::redirect(
            &format!("/transactions/{id}"),
            "Splits saved. Reports count the split amounts.",
        ));
    }
    if action == "unsplit" {
        let id = id.unwrap();
        let t = db::owned(conn, "transactions", user, id).await?;
        if t.id("parent_id") != 0 {
            bail!("Choose the original transaction")
        };
        sqlx::query("DELETE FROM transactions WHERE user_id=$1 AND parent_id=$2")
            .bind(user)
            .bind(id)
            .execute(&mut *conn)
            .await?;
        db::save(
            conn,
            "transactions",
            user,
            Some(id),
            json!({"is_split":false,"split_needs_review":false}),
        )
        .await?;
        return Ok(web::redirect(
            &format!("/transactions/{id}"),
            "Original transaction restored.",
        ));
    }
    let fields = fields(p, "transaction");
    let mut t = if let Some(id) = id {
        db::owned(conn, "transactions", user, id).await?
    } else {
        json!({"user_id":user,"source":"manual","date":domain::today(),"iso_currency_code":"USD","tags":""})
    };
    let mut attrs = typed(select(
        &fields,
        &[
            "name",
            "date",
            "amount",
            "iso_currency_code",
            "source_account_name",
            "category_id",
            "notes",
            "tags",
            "hidden",
            "duplicate",
            "transfer",
            "card_payment",
            "expense_type",
        ],
    ))?;
    if id.is_some() && (t.s("source") != "manual" || t.flag("is_split") || t.id("parent_id") != 0) {
        for k in [
            "amount",
            "date",
            "account_id",
            "source_account_name",
            "iso_currency_code",
        ] {
            attrs.as_object_mut().unwrap().remove(k);
        }
    }
    owned_category(l, attrs.id("category_id"))?;
    validate_expense(&attrs)?;
    if (attrs.get("category_id").is_some() && attrs.id("category_id") != t.id("category_id"))
        || (fields.s("lock_category") == "1" && !t.flag("category_locked"))
        || fields.s("remember_merchant") == "1"
    {
        merge(
            &mut attrs,
            json!({"category_locked":true,"category_origin":"manual","category_reason":"Your category choice","suggested_category_id":null,"category_suggestion_reason":null}),
        );
    } else if fields.s("lock_category") == "0" && t.flag("category_locked") {
        merge(
            &mut attrs,
            json!({"category_id":null,"category_locked":false,"category_origin":null,"category_reason":null,"suggested_category_id":null,"category_suggestion_reason":null}),
        );
    }
    if !fields.s("classification_choice").is_empty() {
        let choice = fields.s("classification_choice");
        if !["automatic", "regular", "transfer", "card_payment"].contains(&choice) {
            bail!("Choose a valid classification")
        };
        merge(
            &mut attrs,
            json!({"classification_override":choice!="automatic","classification_origin":if choice!="automatic"{Some("manual")}else{None},"classification_reason":if choice!="automatic"{Some("Your manual cash-flow classification")}else{None},"transfer":choice=="transfer","card_payment":choice=="card_payment","matched_transaction_id":null,"suggested_classification":null,"suggestion_reason":null}),
        );
    }
    merge(&mut t, attrs);
    required(&t, "name")?;
    domain::date(t.s("date"))?;
    domain::amount(
        &t["amount"]
            .as_str()
            .map(str::to_string)
            .unwrap_or_else(|| t["amount"].to_string()),
    )?;
    valid_currency(t.s("iso_currency_code"))?;
    t["tags"] = json!(domain::tags(t.s("tags")));
    if id.is_none() {
        for rule in &l.rules {
            domain::apply_rule(rule, &mut t)
        }
    }
    let t = db::save(conn, "transactions", user, id, t).await?;
    if fields.s("remember_merchant") == "1" && t.id("category_id") != 0 {
        if let Some(key) = domain::merchant_key(&t) {
            let existing = l
                .rules
                .iter()
                .find(|r| r.s("merchant_key") == key)
                .map(|r| r.id("id"));
            db::save(conn,"transaction_rules",user,existing,json!({"merchant_key":key,"category_id":t["category_id"],"match_text":t.s("name"),"kind":"regular","effective_from":chrono::Utc::now().naive_utc()})).await?;
        }
    }
    domain::refresh(conn, user).await?;
    Ok(web::redirect(
        &format!(
            "/transactions/{}",
            if t.id("parent_id") != 0 {
                t.id("parent_id")
            } else {
                t.id("id")
            }
        ),
        "Transaction saved; account balances are unchanged.",
    ))
}
use std::collections::HashSet;
pub fn duplicate_groups(l: &Ledger) -> Vec<Vec<i64>> {
    let mut groups = std::collections::BTreeMap::<String, Vec<&Value>>::new();
    for t in &l.transactions {
        if !l.tracked(t)
            || t.id("parent_id") != 0
            || ["duplicate", "pending", "transfer", "card_payment"]
                .iter()
                .any(|key| t.flag(key))
        {
            continue;
        }
        let name = if t.s("merchant_name").is_empty() {
            t.s("name")
        } else {
            t.s("merchant_name")
        };
        let key = format!(
            "{}|{}|{}",
            t.dec("amount").normalize(),
            t.s("iso_currency_code"),
            domain::normalize(name)
        );
        groups.entry(key).or_default().push(t);
    }
    let mut result = vec![];
    for mut entries in groups.into_values() {
        entries.sort_by_key(|t| (std::cmp::Reverse(t.day("date")), t.id("id")));
        let mut by_date = std::collections::BTreeMap::new();
        for t in &entries {
            by_date
                .entry(t.day("date"))
                .or_insert_with(Vec::new)
                .push(t.id("id"));
        }
        result.extend(by_date.into_values().filter(|group| group.len() > 1));
        for bank in entries.iter().filter(|t| t.s("source") == "plaid") {
            for imported in entries.iter().filter(|t| t.s("source") == "import") {
                let distance = (bank.day("date") - imported.day("date")).num_days().abs();
                if (1..=14).contains(&distance) {
                    result.push(vec![bank.id("id"), imported.id("id")]);
                }
            }
        }
    }
    result
}

async fn categories(
    conn: &mut PgConnection,
    user: i64,
    l: &Ledger,
    id: Option<i64>,
    action: &str,
    p: &Value,
) -> Result<Response> {
    if action == "expense_type" {
        let category = id.unwrap();
        owned_category(l, category)?;
        let choice = p.s("expense_type");
        let existing = l.expenses.iter().find(|v| v.id("category_id") == category);
        if choice == "automatic" {
            if let Some(e) = existing {
                db::delete(conn, "category_expense_types", user, e.id("id")).await?;
            }
        } else {
            validate_expense(&json!({"expense_type":choice}))?;
            required(p, "expense_type")?;
            db::save(
                conn,
                "category_expense_types",
                user,
                existing.map(|e| e.id("id")),
                json!({"category_id":category,"expense_type":choice}),
            )
            .await?;
        }
        return Ok(web::redirect(
            "/categories",
            "Expense preference updated for your account.",
        ));
    }
    if action == "merge" {
        let source = id.unwrap();
        owned_category(l, source)?;
        let target = p.id("target_id");
        owned_category(l, target)?;
        if target == 0 {
            bail!("Choose a target category")
        };
        let ids = l.subtree(source);
        if ids.contains(&target) {
            bail!("Choose a category outside the source subtree")
        };
        let affected = l
            .budgets
            .iter()
            .filter(|b| ids.contains(&b.id("category_id")) || b.id("category_id") == target)
            .collect::<Vec<_>>();
        if affected.len() > 1 {
            bail!("Remove overlapping budgets before merging")
        }
        let target_tree = l.subtree(target);
        for b in &l.budgets {
            if !affected.iter().any(|a| a.id("id") == b.id("id"))
                && !l.subtree(b.id("category_id")).is_disjoint(&target_tree)
                && !affected.is_empty()
            {
                bail!("Remove overlapping budgets before merging")
            }
        }
        let ids_vec = ids.iter().copied().collect::<Vec<_>>();
        sqlx::query("UPDATE transactions SET category_id=$1,category_locked=true,category_origin='manual',category_reason='Your category merge',updated_at=now() WHERE user_id=$2 AND category_id=ANY($3)").bind(target).bind(user).bind(&ids_vec).execute(&mut *conn).await?;
        sqlx::query("UPDATE transactions SET suggested_category_id=$1 WHERE user_id=$2 AND suggested_category_id=ANY($3)").bind(target).bind(user).bind(&ids_vec).execute(&mut *conn).await?;
        sqlx::query(
            "UPDATE transaction_rules SET category_id=$1 WHERE user_id=$2 AND category_id=ANY($3)",
        )
        .bind(target)
        .bind(user)
        .bind(&ids_vec)
        .execute(&mut *conn)
        .await?;
        if let Some(b) = affected.first() {
            db::save(
                conn,
                "budgets",
                user,
                Some(b.id("id")),
                json!({"category_id":target}),
            )
            .await?;
        }
        for profile in db::rows(conn, "import_profiles", user).await? {
            let mut settings = profile["settings"].clone();
            if let Some(map) = settings["categories"].as_object_mut() {
                for v in map.values_mut() {
                    if ids.contains(
                        &v.as_str()
                            .and_then(|s| s.parse().ok())
                            .or_else(|| v.as_i64())
                            .unwrap_or(0),
                    ) {
                        *v = json!(target.to_string())
                    }
                }
            }
            db::save(
                conn,
                "import_profiles",
                user,
                Some(profile.id("id")),
                json!({"settings":settings}),
            )
            .await?;
        }
        let mut personal = l
            .categories
            .iter()
            .filter(|c| ids.contains(&c.id("id")) && c.id("user_id") == user)
            .collect::<Vec<_>>();
        personal.sort_by_key(|c| std::cmp::Reverse(l.category_chain(c.id("id")).len()));
        for c in personal {
            db::delete(conn, "categories", user, c.id("id")).await?;
        }
        domain::refresh(conn, user).await?;
        return Ok(web::redirect(
            "/categories",
            "Your category usage and saved mappings were merged.",
        ));
    }
    let attrs = typed(select(&fields(p, "category"), &["name", "parent_id"]))?;
    required(&attrs, "name")?;
    owned_category(l, attrs.id("parent_id"))?;
    if let Some(id) = id {
        if l.category(id).is_none_or(|c| c.id("user_id") != user) {
            bail!("Shared categories cannot be edited")
        };
        if l.subtree(id).contains(&attrs.id("parent_id")) {
            bail!("A category cannot be its own ancestor")
        }
    }
    if l.categories.iter().any(|c| {
        c.id("user_id") == user
            && Some(c.id("id")) != id
            && c.s("name") == attrs.s("name")
            && c.id("parent_id") == attrs.id("parent_id")
    }) {
        bail!("Category already exists")
    }
    db::save(conn, "categories", user, id, attrs).await?;
    if let Some(id) = id {
        let ids = l.subtree(id).into_iter().collect::<Vec<_>>();
        sqlx::query("UPDATE transactions SET category_locked=true,category_origin='manual',category_reason='Your category edit' WHERE user_id=$1 AND category_id=ANY($2)").bind(user).bind(ids).execute(&mut *conn).await?;
    }
    domain::refresh(conn, user).await?;
    Ok(web::redirect("/categories", "Category saved."))
}
async fn rules(
    conn: &mut PgConnection,
    user: i64,
    l: &Ledger,
    id: Option<i64>,
    method: &str,
    p: &Value,
) -> Result<Response> {
    if method == "DELETE" {
        db::delete(conn, "transaction_rules", user, id.unwrap()).await?;
        return Ok(web::redirect(
            "/transaction_rules",
            "Rule removed; existing edits are preserved.",
        ));
    }
    let mut rule = if let Some(id) = id {
        db::owned(conn, "transaction_rules", user, id).await?
    } else {
        json!({"user_id":user,"kind":"regular","description_operator":"contains","tags":""})
    };
    merge(
        &mut rule,
        typed(select(
            &fields(p, "transaction_rule"),
            &[
                "match_text",
                "description_operator",
                "account_id",
                "source_account_name",
                "category_id",
                "kind",
                "tags",
            ],
        ))?,
    );
    owned_category(l, rule.id("category_id"))?;
    owned_account(l, rule.id("account_id"))?;
    if rule.s("match_text").trim().is_empty()
        && rule.s("merchant_key").is_empty()
        && rule.id("account_id") == 0
        && rule.s("source_account_name").trim().is_empty()
    {
        bail!("Add a description or account condition")
    }
    if rule.id("account_id") != 0 && !rule.s("source_account_name").is_empty() {
        bail!("Choose a linked account or an imported account, not both")
    };
    if !["contains", "equals", "starts_with", "ends_with"].contains(&rule.s("description_operator"))
        || !["regular", "transfer", "card_payment", "hidden"].contains(&rule.s("kind"))
    {
        bail!("Choose a valid rule condition and action")
    }
    if p.s("preview") == "1" {
        let matches = l
            .transactions
            .iter()
            .filter(|t| t.id("parent_id") == 0 && domain::rule_matches(&rule, t))
            .map(|t| l.decorated(t))
            .collect::<Vec<_>>();
        return Ok(Json(json!({"matches":matches.len(),"transactions":matches.iter().take(10).collect::<Vec<_>>(),"protected_categories":matches.iter().filter(|t|t.flag("category_locked")&&["manual","import","existing","split"].contains(&t.s("category_origin"))).count()})).into_response());
    }
    rule["effective_from"] = json!(chrono::Utc::now().naive_utc());
    let rule = db::save(conn, "transaction_rules", user, id, rule).await?;
    if p.s("apply_existing") == "1" {
        for t in l.transactions.iter().filter(|t| t.id("parent_id") == 0) {
            let mut changed = t.clone();
            domain::apply_rule(&rule, &mut changed);
            if &changed != t {
                db::save(conn, "transactions", user, Some(t.id("id")), changed).await?;
            }
        }
    }
    domain::refresh(conn, user).await?;
    Ok(web::redirect("/transaction_rules", "Rule saved."))
}
async fn budgets(
    conn: &mut PgConnection,
    user: i64,
    l: &Ledger,
    id: Option<i64>,
    method: &str,
    p: &Value,
) -> Result<Response> {
    if method == "DELETE" {
        db::delete(conn, "budgets", user, id.unwrap()).await?;
        return Ok(web::redirect("/budgets", "Budget removed."));
    }
    let mut b = if let Some(id) = id {
        db::owned(conn, "budgets", user, id).await?
    } else {
        json!({"currency":"USD","rollover":false})
    };
    let original = b.clone();
    merge(
        &mut b,
        typed(select(
            &fields(p, "budget"),
            &["category_id", "amount", "currency", "starts_on", "rollover"],
        ))?,
    );
    owned_category(l, b.id("category_id"))?;
    if b.id("category_id") == 0 {
        bail!("Choose a category")
    };
    let value = domain::amount(b.s("amount"))?;
    if value < rust_decimal::Decimal::ZERO || value.scale() > 2 {
        bail!("Budget must be nonnegative with at most two decimal places")
    };
    valid_currency(b.s("currency"))?;
    let start = domain::month_start(domain::date(b.s("starts_on"))?);
    if id.is_some() && start != original.day("starts_on") {
        bail!("Start month cannot change; create a new budget instead")
    }
    b["starts_on"] = json!(start);
    let ids = l.subtree(b.id("category_id"));
    if l.budgets.iter().any(|other| {
        Some(other.id("id")) != id && !l.subtree(other.id("category_id")).is_disjoint(&ids)
    }) {
        bail!("Category overlaps an existing parent or child budget")
    }
    db::save(conn, "budgets", user, id, b).await?;
    Ok(web::redirect("/budgets", "Recurring budget saved."))
}
async fn assets(
    conn: &mut PgConnection,
    user: i64,
    id: Option<i64>,
    p: &Value,
) -> Result<Response> {
    let mut a = if let Some(id) = id {
        db::owned(conn, "manual_assets", user, id).await?
    } else {
        json!({"currency":"USD"})
    };
    let old = a.clone();
    merge(
        &mut a,
        select(
            &fields(p, "manual_asset"),
            &[
                "name",
                "asset_type",
                "value",
                "currency",
                "valued_on",
                "notes",
            ],
        ),
    );
    required(&a, "name")?;
    required(&a, "asset_type")?;
    if !["property", "vehicle", "other"].contains(&a.s("asset_type")) {
        bail!("Choose a valid asset type")
    }
    valid_currency(a.s("currency"))?;
    let value = domain::amount(a.s("value"))?;
    if value < rust_decimal::Decimal::ZERO || value.scale() > 2 {
        bail!("Value must be nonnegative with at most two decimal places")
    };
    let day = domain::date(a.s("valued_on"))?;
    if day > domain::today() || (id.is_some() && day < old.day("valued_on")) {
        bail!("Valuation date cannot be future or earlier than the current valuation")
    };
    if id.is_some() && old.s("currency") != a.s("currency") {
        bail!("Asset currency cannot change after valuations are recorded")
    }
    let a = db::save(conn, "manual_assets", user, id, a).await?;
    sqlx::query("INSERT INTO asset_valuations(manual_asset_id,value,valued_on) VALUES($1,$2,$3) ON CONFLICT(manual_asset_id,valued_on) DO UPDATE SET value=excluded.value,updated_at=now()").bind(a.id("id")).bind(value).bind(day).execute(conn).await?;
    Ok(web::redirect("/my-assets", "Asset valuation saved."))
}
async fn recurring(
    conn: &mut PgConnection,
    user: i64,
    l: &Ledger,
    id: Option<i64>,
    path: &str,
    method: &str,
    p: &Value,
) -> Result<Response> {
    if path == "/recurring/dismiss" {
        let t = l
            .transaction(p.id("transaction_id"))
            .ok_or_else(|| anyhow::anyhow!("Transaction is unavailable"))?;
        let key = domain::detection_key(t)
            .ok_or_else(|| anyhow::anyhow!("No distinct merchant pattern"))?;
        if !l.dismissals.iter().any(|r| r.s("detection_key") == key) {
            db::save(
                conn,
                "recurring_dismissals",
                user,
                None,
                json!({"name":t.s("name"),"detection_key":key}),
            )
            .await?;
        }
        return Ok(web::redirect("/recurring", "Suggestion dismissed."));
    }
    if path.starts_with("/recurring/dismissals/") {
        let id = path.rsplit('/').next().unwrap().parse()?;
        db::delete(conn, "recurring_dismissals", user, id).await?;
        return Ok(web::redirect("/recurring", "Suggestion restored."));
    }
    if method == "DELETE" {
        let item = db::owned(conn, "recurring_items", user, id.unwrap()).await?;
        if !item.s("detection_key").is_empty()
            && !l
                .dismissals
                .iter()
                .any(|d| d.s("detection_key") == item.s("detection_key"))
        {
            db::save(
                conn,
                "recurring_dismissals",
                user,
                None,
                select(&item, &["name", "detection_key"]),
            )
            .await?;
        }
        db::delete(conn, "recurring_items", user, id.unwrap()).await?;
        return Ok(web::redirect(
            "/recurring",
            "Tracking stopped; recorded transactions are unchanged.",
        ));
    }
    let mut item = if let Some(id) = id {
        db::owned(conn, "recurring_items", user, id).await?
    } else {
        json!({"frequency":"monthly","direction":"expense","status":"active","currency":"USD","month_end":false,"next_on":domain::today()})
    };
    if id.is_none() && p.id("transaction_id") != 0 {
        let t = l
            .transaction(p.id("transaction_id"))
            .ok_or_else(|| anyhow::anyhow!("Transaction is unavailable"))?;
        merge(
            &mut item,
            json!({"merchant_key":domain::merchant_key(t),"detection_key":domain::detection_key(t),"currency":t.s("iso_currency_code"),"direction":if t.dec("amount")<rust_decimal::Decimal::ZERO{"income"}else{"expense"},"account_id":t["account_id"],"source_account_name":t["source_account_name"]}),
        );
    }
    let mut attrs = typed(select(
        &fields(p, "recurring_item"),
        &[
            "name",
            "amount",
            "currency",
            "direction",
            "frequency",
            "next_on",
            "month_end",
            "account_id",
            "status",
            "notes",
        ],
    ))?;
    if !item.s("merchant_key").is_empty() {
        for k in ["currency", "direction", "account_id"] {
            attrs.as_object_mut().unwrap().remove(k);
        }
    }
    merge(&mut item, attrs);
    required(&item, "name")?;
    if item.s("name").chars().count() > 120 {
        bail!("Name is too long")
    };
    valid_currency(item.s("currency"))?;
    owned_account(l, item.id("account_id"))?;
    let value = domain::decimal(&item["amount"])?;
    if value <= rust_decimal::Decimal::ZERO
        || value >= rust_decimal::Decimal::from(10_000_000_000i64)
        || value.scale() > 4
    {
        bail!("Choose a positive amount with at most four decimal places")
    };
    if !["weekly", "biweekly", "monthly", "quarterly", "yearly"].contains(&item.s("frequency"))
        || !["expense", "income"].contains(&item.s("direction"))
        || !["active", "paused"].contains(&item.s("status"))
    {
        bail!("Choose valid recurring settings")
    };
    let day = domain::date(item.s("next_on"))?;
    if day < domain::date("1900-01-01")?
        || day
            > domain::today()
                .checked_add_months(Months::new(240))
                .unwrap()
    {
        bail!("Date must be between 1900 and twenty years from now")
    };
    if item.flag("month_end") && !["weekly", "biweekly"].contains(&item.s("frequency")) {
        item["next_on"] = json!(domain::month_end(day))
    }
    db::save(conn, "recurring_items", user, id, item).await?;
    Ok(web::redirect(
        "/recurring",
        "Recurring item saved; recorded transactions are unchanged.",
    ))
}
