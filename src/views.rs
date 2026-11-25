use crate::{
    actions, db,
    domain::{self, Ledger, Record},
    imports,
    web::{AppState, Session},
};
use anyhow::{Result, bail};
use axum::{
    http::{StatusCode, header},
    response::{Html, IntoResponse, Response},
};
use chrono::{Duration, Months};
use rust_decimal::Decimal;
use serde_json::{Value, json};

pub async fn page(
    state: &AppState,
    session: &Session,
    path: &str,
    p: &Value,
    error: Option<&str>,
) -> Result<Response> {
    let user = session.user.as_ref().map(|u| u.id("id")).unwrap_or(0);
    let mut conn = state.pool.acquire().await?;
    let l = if user == 0 {
        Ledger::default()
    } else {
        Ledger::load(&mut conn, user).await?
    };
    let mut categories = l
        .categories
        .iter()
        .map(|c| {
            let mut v = c.clone();
            v["display_name"] = json!(l.category_name(c.id("id")));
            v["expense_type"] = json!(l.category_expense(c.id("id")));
            v
        })
        .collect::<Vec<_>>();
    categories.sort_by(|a, b| a.s("display_name").cmp(b.s("display_name")));
    let parts = path.trim_matches('/').split('/').collect::<Vec<_>>();
    let section = parts[0];
    let id = parts.get(1).and_then(|s| s.parse::<i64>().ok());
    let mut context = json!({"path":path,"params":p,"csrf":session.csrf,"signed_in":user!=0,"user_id":user,"categories":categories,"accounts":l.accounts,"assets":l.assets,"today":domain::today(),"currencies":domain::CURRENCIES,"flags":domain::FLAGS,"notice":p["notice"],"draft":{},"screen":"dashboard","title":"Overview","form_action":"","form_method":"POST"});
    let nav = vec![
        ("/", "Overview"),
        ("/transactions", "Transactions"),
        ("/budgets", "Budgets"),
        ("/recurring", "Recurring"),
        ("/trends", "Trends"),
        ("/categories", "Categories"),
        ("/my-assets", "Assets"),
        ("/imports/new", "Import data"),
    ];
    context["navigation"]=json!(nav.iter().map(|(href,label)|json!({"href":href,"label":label,"active":if *href=="/"{path=="/"}else{path.starts_with(href.trim_end_matches("/new"))}})).collect::<Vec<_>>());
    match section {
        "sign_in" | "sign_up" | "password" => {
            context["screen"] = json!("auth");
            context["auth_kind"] = json!(section);
            context["title"] = json!(match section {
                "sign_up" => "Create an account",
                "password" => "Change password",
                _ => "Sign in",
            });
        }
        "" => {
            let currency = if domain::CURRENCIES.contains(&p.s("currency")) {
                p.s("currency")
            } else {
                "USD"
            };
            let report = l.report(
                domain::month_start(domain::today()),
                domain::today(),
                currency,
            );
            context["report"] = report;
            context["currency"] = json!(currency);
            let mut totals = std::collections::BTreeMap::<String, Decimal>::new();
            for a in &l.accounts {
                if a.flag("tracked") && !a.flag("net_worth_excluded") {
                    *totals
                        .entry(a.s("iso_currency_code").to_string())
                        .or_default() += domain::signed_balance(a, a.dec("current_balance"));
                }
            }
            for a in &l.assets {
                *totals.entry(a.s("currency").to_string()).or_default() += a.dec("value")
            }
            context["net_worth"]=json!(totals.into_iter().map(|(currency,amount)|json!({"currency":currency,"amount":amount.to_string()})).collect::<Vec<_>>());
            context["recent_transactions"] = json!(
                l.search(&json!({}))?
                    .into_iter()
                    .take(8)
                    .collect::<Vec<_>>()
            );
            let items:Vec<Value>=sqlx::query_scalar("SELECT to_jsonb(p)-'access_token'-'sync_request_token'||jsonb_build_object('institution_name',i.name,'queued',EXISTS(SELECT 1 FROM sync_jobs j WHERE j.plaid_item_id=p.id)) FROM plaid_items p JOIN institutions i ON i.id=p.institution_id WHERE p.user_id=$1 ORDER BY i.name").bind(user).fetch_all(&mut *conn).await?;
            context["connections"] = json!(items);
        }
        "transactions" | "transactions.csv" => {
            context["title"] = json!("Transactions");
            context["screen"] = json!("transactions");
            context["rules_link"] = json!(true);
            if path.ends_with("/duplicates") {
                context["screen"] = json!("duplicates");
                context["title"] = json!("Review duplicates");
                context["groups"] = json!(
                    actions::duplicate_groups(&l)
                        .iter()
                        .map(|g| g
                            .iter()
                            .filter_map(|id| l.transaction(*id))
                            .map(|t| l.decorated(t))
                            .collect::<Vec<_>>())
                        .collect::<Vec<_>>()
                );
            } else if path.ends_with("/new") || id.is_some() {
                context["screen"] = json!("transaction");
                let draft = if let Some(id) = id {
                    let t = l
                        .transaction(id)
                        .ok_or_else(|| anyhow::anyhow!("Record not found"))?;
                    if t.id("parent_id") != 0 {
                        return Ok(axum::response::Redirect::to(&format!(
                            "/transactions/{}",
                            t.id("parent_id")
                        ))
                        .into_response());
                    }
                    l.decorated(t)
                } else {
                    json!({"source":"manual","date":domain::today(),"iso_currency_code":"USD","name":"","amount":"","tags":""})
                };
                let root = draft.id("id");
                context["splits"] = json!(
                    l.transactions
                        .iter()
                        .filter(|t| t.id("parent_id") == root && root != 0)
                        .map(|t| l.decorated(t))
                        .collect::<Vec<_>>()
                );
                context["form_action"] = json!(if let Some(id) = id {
                    format!("/transactions/{id}")
                } else {
                    "/transactions".into()
                });
                context["title"] = json!(if id.is_some() {
                    "Transaction details"
                } else {
                    "Add transaction"
                });
                context["draft"] = draft;
            } else {
                let rows = match l.search(p) {
                    Ok(rows) => rows,
                    Err(_) if error.is_some() => vec![],
                    Err(e) => return Err(e),
                };
                if section == "transactions.csv" || p.s("format") == "csv" {
                    return export(&rows);
                }
                let count = rows.len();
                let per_page = match p.id("per_page") {
                    25 => 25,
                    100 => 100,
                    _ => 50,
                };
                let pages = count.div_ceil(per_page).max(1);
                let page = p.id("page").max(1).min(pages as i64) as usize;
                context["rows"] = json!(
                    rows.into_iter()
                        .skip((page - 1) * per_page)
                        .take(per_page)
                        .collect::<Vec<_>>()
                );
                context["total_count"] = json!(count);
                context["page"] = json!(page);
                context["pages"] = json!(pages);
                context["per_page"] = json!(per_page);
                let mut pairs = p.as_object().cloned().unwrap_or_default();
                pairs.remove("page");
                pairs.remove("notice");
                context["filter_query"] = json!(
                    url::form_urlencoded::Serializer::new(String::new())
                        .extend_pairs(pairs.iter().filter_map(|(k, v)| v.as_str().map(|s| (k, s))))
                        .finish()
                );
            }
        }
        "categories" => {
            context["screen"] = json!("categories");
            context["title"] = json!("Categories");
            context["form_action"] = json!("/categories");
            if let Some(id) = id {
                context["draft"] = db::owned(&mut conn, "categories", user, id).await?;
                context["form_action"] = json!(format!("/categories/{id}"));
            }
            let q = domain::normalize(p.s("q"));
            context["visible_categories"] = json!(
                context["categories"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter(|c| (q.is_empty()
                        || domain::normalize(c.s("display_name")).contains(&q))
                        && (p.s("kind").is_empty()
                            || if p.s("kind") == "system" {
                                c.flag("system")
                            } else {
                                c.id("user_id") == user
                            }))
                    .collect::<Vec<_>>()
            );
        }
        "transaction_rules" => {
            context["screen"] = json!("rules");
            context["title"] = json!("Transaction rules");
            context["rules"] = json!(
                l.rules
                    .iter()
                    .filter(|r| {
                        let q = domain::normalize(p.s("q"));
                        (q.is_empty()
                            || domain::normalize(&format!(
                                "{} {} {} {}",
                                r.s("match_text"),
                                r.s("source_account_name"),
                                r.s("tags"),
                                l.category_name(r.id("category_id"))
                            ))
                            .contains(&q))
                            && (p.s("kind").is_empty() || r.s("kind") == p.s("kind"))
                    })
                    .map(|r| {
                        let mut v = r.clone();
                        v["category_name"] = json!(l.category_name(r.id("category_id")));
                        v
                    })
                    .collect::<Vec<_>>()
            );
            context["form_action"] = json!("/transaction_rules");
            context["draft"] = json!({"description_operator":"contains","kind":"regular"});
            if let Some(id) = id {
                context["draft"] = db::owned(&mut conn, "transaction_rules", user, id).await?;
                context["form_action"] = json!(format!("/transaction_rules/{id}"));
            }
        }
        "budgets" => {
            context["screen"] = json!("budgets");
            context["title"] = json!("Monthly budgets");
            let month = if p.s("month").is_empty() {
                domain::month_start(domain::today())
            } else {
                domain::date(&format!("{}-01", p.s("month")))?
            };
            context["month"] = json!(month.format("%Y-%m").to_string());
            context["budgets"] = json!(
                l.budgets
                    .iter()
                    .map(|b| l.budget_summary(b, month))
                    .collect::<Vec<_>>()
            );
            context["draft"] = json!({"currency":"USD","starts_on":month});
            context["form_action"] = json!("/budgets");
            if let Some(id) = id {
                context["draft"] = db::owned(&mut conn, "budgets", user, id).await?;
                context["form_action"] = json!(format!("/budgets/{id}"));
            }
        }
        "my-assets" => {
            context["screen"] = json!("assets");
            context["visible_assets"] = json!(
                l.assets
                    .iter()
                    .filter(|a| (p.s("q").is_empty()
                        || domain::normalize(&format!("{} {}", a.s("name"), a.s("notes")))
                            .contains(&domain::normalize(p.s("q"))))
                        && (p.s("kind").is_empty() || a.s("asset_type") == p.s("kind")))
                    .collect::<Vec<_>>()
            );
            context["title"] = json!("My assets");
            context["form_action"] = json!("/my-assets");
            context["draft"] =
                json!({"currency":"USD","asset_type":"property","valued_on":domain::today()});
            if let Some(id) = id {
                context["draft"] = db::owned(&mut conn, "manual_assets", user, id).await?;
                context["form_action"] = json!(format!("/my-assets/{id}"));
                context["valuations"] = json!(
                    l.valuations
                        .iter()
                        .filter(|v| v.id("manual_asset_id") == id)
                        .collect::<Vec<_>>()
                );
            }
        }
        "recurring" => {
            context["screen"] = json!("recurring");
            context["title"] = json!("Bills and subscriptions");
            context["items"] = json!(l.recurring);
            context["dismissals"] = json!(l.dismissals);
            let suggestions = l.recurring_suggestions(domain::today());
            let pages = suggestions.len().div_ceil(8).max(1);
            let page = p.id("suggestion_page").max(1).min(pages as i64) as usize;
            context["suggestion_count"] = json!(suggestions.len());
            context["suggestion_page"] = json!(page);
            context["suggestion_pages"] = json!(pages);
            context["suggestions"] = json!(
                suggestions
                    .into_iter()
                    .skip((page - 1) * 8)
                    .take(8)
                    .collect::<Vec<_>>()
            );
            let mut upcoming = vec![];
            let mut totals = std::collections::BTreeMap::<(String, String), Decimal>::new();
            for item in &l.recurring {
                if item.id("account_id") != 0
                    && !l
                        .account(item.id("account_id"))
                        .is_some_and(|a| a.flag("tracked"))
                {
                    continue;
                }
                for date in
                    domain::occurrences(item, domain::today(), domain::today() + Duration::days(29))
                {
                    upcoming.push(json!({"date":date,"item":item}));
                    *totals
                        .entry((item.s("direction").into(), item.s("currency").into()))
                        .or_default() += item.dec("amount")
                }
            }
            upcoming.sort_by(|a, b| a.s("date").cmp(b.s("date")));
            context["upcoming"] = json!(upcoming);
            context["recurring_totals"]=json!(totals.into_iter().map(|((direction,currency),amount)|json!({"direction":direction,"currency":currency,"amount":amount.to_string()})).collect::<Vec<_>>());
            context["draft"] = json!({"currency":"USD","direction":"expense","frequency":"monthly","next_on":domain::today(),"status":"active"});
            context["form_action"] = json!("/recurring");
            if let Some(id) = id {
                let draft = db::owned(&mut conn, "recurring_items", user, id).await?;
                context["linked_history"] = json!(
                    l.transactions
                        .iter()
                        .filter(|t| !draft.s("merchant_key").is_empty()
                            && domain::merchant_key(t).as_deref() == Some(draft.s("merchant_key"))
                            && t.s("iso_currency_code") == draft.s("currency")
                            && t.id("account_id") == draft.id("account_id")
                            && (t.id("account_id") != 0
                                || t.s("source_account_name") == draft.s("source_account_name"))
                            && l.reportable(t))
                        .rev()
                        .take(6)
                        .map(|t| l.decorated(t))
                        .collect::<Vec<_>>()
                );
                context["draft"] = draft;
                context["form_action"] = json!(format!("/recurring/{id}"));
            } else if p.id("transaction_id") != 0 {
                let t = l
                    .transaction(p.id("transaction_id"))
                    .ok_or_else(|| anyhow::anyhow!("Transaction is unavailable"))?;
                let frequency = if ["weekly", "biweekly", "monthly", "quarterly", "yearly"]
                    .contains(&p.s("frequency"))
                {
                    p.s("frequency")
                } else {
                    "monthly"
                };
                let mut draft = json!({"name":if t.s("merchant_name").is_empty(){t.s("name")}else{t.s("merchant_name")},"amount":t.dec("amount").abs().to_string(),"currency":t.s("iso_currency_code"),"direction":if t.dec("amount")<Decimal::ZERO{"income"}else{"expense"},"frequency":frequency,"next_on":t.s("date"),"month_end":t.day("date")==domain::month_end(t.day("date")),"status":"active","account_id":t["account_id"],"merchant_key":domain::merchant_key(t)});
                let from = domain::today().max(t.day("date") + Duration::days(1));
                let dates = domain::occurrences(&draft, from, from + Duration::days(400));
                if let Some(next) = dates.first() {
                    draft["next_on"] = json!(next)
                }
                context["draft"] = draft;
                context["transaction_id"] = json!(p.id("transaction_id"));
            }
        }
        "trends" => {
            context["screen"] = json!("trends");
            context["title"] = json!("Trends");
            let currency = if domain::CURRENCIES.contains(&p.s("currency")) {
                p.s("currency")
            } else {
                "USD"
            };
            let from = if !p.s("month").is_empty() {
                domain::date(&format!("{}-01", p.s("month")))?
            } else if !p.s("from").is_empty() {
                domain::date(p.s("from"))?
            } else {
                domain::month_start(domain::today())
                    .checked_sub_months(Months::new(if p.s("spending_view") == "cost_type" {
                        0
                    } else {
                        5
                    }))
                    .unwrap()
            };
            let to = if !p.s("month").is_empty() {
                domain::month_end(from).min(domain::today())
            } else if !p.s("to").is_empty() {
                domain::date(p.s("to"))?
            } else {
                domain::today()
            };
            if from > to || (to - from).num_days() > 7305 {
                bail!("Choose a date range of up to twenty years")
            };
            let mut filters = p.clone();
            filters["from"] = json!(from);
            filters["to"] = json!(to);
            filters["currency"] = json!(currency);
            filters.as_object_mut().unwrap().remove("flag");
            let filtered = l
                .search(&filters)?
                .iter()
                .map(|t| t.id("id"))
                .collect::<std::collections::HashSet<_>>();
            let mut report_ledger = l.clone();
            report_ledger
                .transactions
                .retain(|t| t.flag("is_split") || filtered.contains(&t.id("id")));
            context["report"] = report_ledger.report(from, to, currency);
            context["history"] = json!(l.net_worth(currency, from, to));
            context["currency"] = json!(currency);
            context["from"] = json!(from);
            context["to"] = json!(to);
        }
        "imports" => {
            context["screen"] = json!("imports");
            context["title"] = json!("Import data");
            context["profiles"] = json!(db::rows(&mut conn, "import_profiles", user).await?);
            let batches = db::rows(&mut conn, "import_batches", user).await?;
            context["recent_imports"] = json!(
                batches
                    .iter()
                    .rev()
                    .filter(|b| !b["imported_at"].is_null())
                    .take(10)
                    .map(|b| actions::select(b, &["id", "imported_at", "settings"]))
                    .collect::<Vec<_>>()
            );
            if let Some(id) = id {
                let batch = db::owned(&mut conn, "import_batches", user, id).await?;
                if batch["imported_at"].is_null() {
                    let mut preview = if batch["settings"].s("format") == "tiller" {
                        imports::tiller_preview(&mut conn, &batch, &l).await?
                    } else {
                        imports::csv_preview(&batch, &l)?
                    };
                    let rows = preview["rows"]
                        .as_array()
                        .cloned()
                        .unwrap_or_default()
                        .into_iter()
                        .filter(|r| match p.s("review") {
                            "duplicates" => {
                                r.flag("duplicate") && !r.flag("existing") && !r.flag("conflict")
                            }
                            "changes" => r.flag("conflict"),
                            _ => true,
                        })
                        .collect::<Vec<_>>();
                    let per_page = if batch["settings"].s("format") == "tiller" {
                        50
                    } else {
                        rows.len().max(1)
                    };
                    let pages = rows.len().div_ceil(per_page).max(1);
                    let page = p.id("page").max(1).min(pages as i64) as usize;
                    preview["rows"] = json!(
                        rows.into_iter()
                            .skip((page - 1) * per_page)
                            .take(per_page)
                            .collect::<Vec<_>>()
                    );
                    context["preview_pages"] = json!(pages);
                    context["preview_page"] = json!(page);
                    context["preview"] = preview;
                }
                context["batch"] = batch;
                context["title"] = json!("Review import");
            }
        }
        "bank_connections" => {
            context["screen"] = json!("bank");
            context["title"] = json!("Connect a bank");
            context["plaid_configured"] = json!(
                std::env::var("PLAID_CLIENT_ID").is_ok() && std::env::var("PLAID_SECRET").is_ok()
            );
        }
        _ => {
            return Ok((
                StatusCode::NOT_FOUND,
                Html("<h1>Page not found · Worth</h1><a href='/'>Overview</a>"),
            )
                .into_response());
        }
    }
    context["error"] = json!(error);
    if error.is_some() && p.flag("_recover_form") {
        let form_key = match section {
            "transactions" => "transaction",
            "categories" => "category",
            "transaction_rules" => "transaction_rule",
            "budgets" => "budget",
            "my-assets" => "manual_asset",
            "recurring" => "recurring_item",
            _ => "",
        };
        let submitted = actions::fields(p, form_key);
        if let Some(map) = submitted.as_object() {
            for (key, value) in map {
                if [
                    "authenticity_token",
                    "password",
                    "password_confirmation",
                    "current_password",
                    "_recover_form",
                ]
                .contains(&key.as_str())
                {
                    continue;
                }
                let restored = if [
                    "hidden",
                    "duplicate",
                    "transfer",
                    "card_payment",
                    "month_end",
                    "rollover",
                    "category_locked",
                ]
                .contains(&key.as_str())
                {
                    json!(submitted.flag(key))
                } else {
                    value.clone()
                };
                context["draft"][key] = restored;
            }
        }
        if let Some(rows) = p["splits"].as_object() {
            context["splits"] = json!(rows.values().collect::<Vec<_>>());
        }
    }
    drop(conn);
    let html = state
        .templates
        .get_template("app.html")?
        .render(template_value(&context))
        .map_err(|e| {
            use std::error::Error;
            let mut text = e.to_string();
            let mut source = e.source();
            while let Some(cause) = source {
                text.push_str(&format!(": {cause}"));
                source = cause.source();
            }
            anyhow::anyhow!(text)
        })?;
    Ok(Html(html).into_response())
}
fn export(rows: &[Value]) -> Result<Response> {
    let mut writer = csv::Writer::from_writer(vec![]);
    let keys = [
        "date",
        "name",
        "amount",
        "iso_currency_code",
        "category_name",
        "account_name",
        "notes",
        "tags",
        "hidden",
        "duplicate",
        "transfer",
        "card_payment",
        "pending",
        "split_needs_review",
        "parent_id",
        "classification_origin",
        "classification_reason",
        "suggested_classification",
        "category_origin",
        "category_reason",
        "category_suggestion_reason",
        "expense_label",
    ];
    writer.write_record([
        "Date",
        "Description",
        "Amount",
        "Currency",
        "Category",
        "Account",
        "Notes",
        "Tags",
        "Hidden",
        "Duplicate",
        "Transfer",
        "CardPayment",
        "Pending",
        "SplitNeedsReview",
        "ParentID",
        "ClassificationOrigin",
        "ClassificationReason",
        "SuggestedClassification",
        "CategoryOrigin",
        "CategoryReason",
        "SuggestedCategory",
        "ExpenseType",
    ])?;
    for row in rows {
        writer.write_record(keys.iter().map(|k| {
            let v = &row[*k];
            let s = if v.is_null() {
                String::new()
            } else if let Some(s) = v.as_str() {
                s.to_string()
            } else {
                v.to_string()
            };
            if *k != "amount" && s.starts_with(['=', '+', '-', '@', '\t', '\r']) {
                format!("'{s}")
            } else {
                s
            }
        }))?
    }
    let bytes = writer.into_inner()?;
    Ok((
        [
            (header::CONTENT_TYPE, "text/csv; charset=utf-8"),
            (
                header::CONTENT_DISPOSITION,
                "attachment; filename=\"worth-transactions.csv\"",
            ),
        ],
        bytes,
    )
        .into_response())
}

// Preserve decimal strings while translating arbitrary-precision JSON numbers.
// Passing serde_json::Value directly to a non-JSON serializer encodes numbers
// as a private map rather than a numeric value.
fn template_value(value: &Value) -> minijinja::Value {
    match value {
        Value::Null => minijinja::Value::from(()),
        Value::Bool(v) => minijinja::Value::from(*v),
        Value::String(v) => minijinja::Value::from(v.clone()),
        Value::Number(v) => {
            if let Some(n) = v.as_i64() {
                minijinja::Value::from(n)
            } else {
                minijinja::Value::from(v.to_string())
            }
        }
        Value::Array(v) => v.iter().map(template_value).collect(),
        Value::Object(v) => v
            .iter()
            .map(|(k, v)| (k.clone(), template_value(v)))
            .collect(),
    }
}
