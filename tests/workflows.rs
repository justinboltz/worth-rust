use anyhow::Result;
use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sqlx::{PgPool, postgres::PgPoolOptions};
use tower::ServiceExt;
use worth::{
    db,
    domain::{self, Ledger, Record},
    imports, plaid,
    web::{self, AppState},
};
struct Browser {
    app: Router,
    cookie: String,
    csrf: String,
}
impl Browser {
    async fn request(
        &mut self,
        method: &str,
        path: &str,
        body: String,
        content_type: &str,
    ) -> (StatusCode, String, String) {
        let mut request = Request::builder()
            .method(method)
            .uri(path)
            .header("host", "localhost:3001")
            .header("content-type", content_type);
        if !self.cookie.is_empty() {
            request = request.header("cookie", &self.cookie)
        }
        let response = self
            .app
            .clone()
            .oneshot(request.body(Body::from(body)).unwrap())
            .await
            .unwrap();
        let status = response.status();
        if let Some(cookie) = response.headers().get("set-cookie") {
            self.cookie = cookie
                .to_str()
                .unwrap()
                .split(';')
                .next()
                .unwrap()
                .to_string()
        }
        let location = response
            .headers()
            .get("location")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let text = String::from_utf8_lossy(&bytes).to_string();
        if let Some(capture) = regex::Regex::new(r#"name="csrf-token" content="([^"]+)""#)
            .unwrap()
            .captures(&text)
        {
            self.csrf = capture[1].to_string()
        }
        (status, text, location)
    }
    async fn get(&mut self, path: &str) -> String {
        let (status, body, _) = self
            .request(
                "GET",
                path,
                String::new(),
                "application/x-www-form-urlencoded",
            )
            .await;
        assert_eq!(status, StatusCode::OK, "GET {path}: {body}");
        body
    }
    async fn post(&mut self, path: &str, values: &[(&str, &str)]) -> (StatusCode, String, String) {
        let mut pairs = values.to_vec();
        pairs.push(("authenticity_token", &self.csrf));
        let body = url::form_urlencoded::Serializer::new(String::new())
            .extend_pairs(pairs)
            .finish();
        self.request("POST", path, body, "application/x-www-form-urlencoded")
            .await
    }
    async fn save(&mut self, path: &str, values: &[(&str, &str)]) -> String {
        let (status, body, location) = self.post(path, values).await;
        assert_eq!(status, StatusCode::SEE_OTHER, "POST {path}: {body}");
        location
    }
    async fn signup(&mut self, email: &str) {
        self.get("/sign_up").await;
        self.save(
            "/sign_up",
            &[
                ("email", email),
                ("password", "correct-horse-battery"),
                ("password_confirmation", "correct-horse-battery"),
            ],
        )
        .await;
        self.get("/").await;
    }
}
#[tokio::test]
async fn complete_workflows_and_user_isolation() -> Result<()> {
    let admin_url = std::env::var("WORTH_TEST_ADMIN_URL")
        .unwrap_or_else(|_| "postgresql://localhost/postgres".into());
    let admin = PgPoolOptions::new()
        .max_connections(2)
        .connect(&admin_url)
        .await?;
    let name = format!("worth_rust_test_{}", uuid::Uuid::new_v4().simple());
    sqlx::query(&format!("CREATE DATABASE {name}"))
        .execute(&admin)
        .await?;
    let mut url: url::Url = admin_url.parse()?;
    url.set_path(&name);
    let pool = PgPool::connect(url.as_str()).await?;
    db::initialize(&pool).await?;
    let state = AppState::new(pool.clone(), [42; 32], false)?;
    let app = web::router(state.clone());
    let mut a = Browser {
        app: app.clone(),
        cookie: String::new(),
        csrf: String::new(),
    };
    a.signup("first@example.test").await;
    let mut b = Browser {
        app: app.clone(),
        cookie: String::new(),
        csrf: String::new(),
    };
    b.signup("second@example.test").await;
    let user: i64 = sqlx::query_scalar("SELECT id FROM users WHERE email='first@example.test'")
        .fetch_one(&pool)
        .await?;
    let second: i64 = sqlx::query_scalar("SELECT id FROM users WHERE email='second@example.test'")
        .fetch_one(&pool)
        .await?;
    let old_cookie = a.cookie.clone();
    let (status, _, _) = a
        .request(
            "POST",
            "/transactions",
            "name=Forbidden&amount=5&date=2026-11-01".into(),
            "application/x-www-form-urlencoded",
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    a.save("/categories", &[("name", "Living")]).await;
    let category: i64 =
        sqlx::query_scalar("SELECT id FROM categories WHERE user_id=$1 AND name='Living'")
            .bind(user)
            .fetch_one(&pool)
            .await?;
    let (status, rejected_create, _) = b
        .post(
            "/transactions",
            &[
                ("name", "Cross user"),
                ("date", "2026-11-01"),
                ("amount", "5"),
                ("category_id", &category.to_string()),
            ],
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(rejected_create.contains("value=\"Cross user\""));
    assert!(rejected_create.contains("role=\"alert\""));
    let location = a
        .save(
            "/transactions",
            &[
                ("name", "Market"),
                ("date", "2026-11-01"),
                ("amount", "100.25"),
                ("category_id", &category.to_string()),
                ("notes", "<script>alert(1)</script>"),
                ("tags", "food, groceries"),
            ],
        )
        .await;
    let transaction: i64 = location
        .split('?')
        .next()
        .unwrap()
        .rsplit('/')
        .next()
        .unwrap()
        .parse()?;
    let html = a.get(&format!("/transactions/{transaction}")).await;
    assert!(html.contains("&lt;script&gt;alert(1)&lt;"), "{}", html);
    assert!(!html.contains("<script>alert(1)</script>"));
    let (status, _, _) = b
        .request(
            "GET",
            &format!("/transactions/{transaction}"),
            String::new(),
            "",
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (status, invalid_split, _) = a
        .post(
            &format!("/transactions/{transaction}/split"),
            &[
                ("splits[0][name]", "Food"),
                ("splits[0][amount]", "50"),
                ("splits[1][name]", "Household"),
                ("splits[1][amount]", "50"),
            ],
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(invalid_split.contains("role=\"alert\""));
    assert!(invalid_split.contains("value=\"Food\""));
    assert!(invalid_split.contains("value=\"Household\""));
    assert!(invalid_split.contains("value=\"50\""));
    a.save(
        &format!("/transactions/{transaction}/split"),
        &[
            ("splits[0][name]", "Food"),
            ("splits[0][amount]", "60.25"),
            ("splits[0][category_id]", &category.to_string()),
            ("splits[1][name]", "Household"),
            ("splits[1][amount]", "40"),
        ],
    )
    .await;
    let mut conn = pool.acquire().await?;
    let l = Ledger::load(&mut conn, user).await?;
    assert_eq!(
        l.report(
            domain::date("2026-11-01")?,
            domain::date("2026-11-30")?,
            "USD"
        )
        .dec("spending"),
        domain::amount("100.25")?
    );
    drop(conn);
    a.save(
        &format!("/transactions/{transaction}"),
        &[("hidden", "1"), ("amount", "999")],
    )
    .await;
    let mut conn = pool.acquire().await?;
    let l = Ledger::load(&mut conn, user).await?;
    assert_eq!(
        l.transaction(transaction).unwrap().dec("amount"),
        domain::amount("100.25")?
    );
    assert_eq!(
        l.report(
            domain::date("2026-11-01")?,
            domain::date("2026-11-30")?,
            "USD"
        )
        .dec("spending"),
        Decimal::ZERO
    );
    drop(conn);
    a.save(&format!("/transactions/{transaction}"), &[("hidden", "0")])
        .await;
    a.save(
        "/budgets",
        &[
            ("category_id", &category.to_string()),
            ("amount", "200"),
            ("currency", "USD"),
            ("starts_on", "2026-11-01"),
            ("rollover", "1"),
        ],
    )
    .await;
    let (status, _, _) = a
        .post(
            "/budgets",
            &[
                ("category_id", &category.to_string()),
                ("amount", "200"),
                ("currency", "USD"),
                ("starts_on", "2026-11-01"),
            ],
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    a.save(
        "/my-assets",
        &[
            ("name", "Home"),
            ("asset_type", "property"),
            ("value", "250000.25"),
            ("currency", "USD"),
            ("valued_on", "2026-11-01"),
        ],
    )
    .await;
    let asset: i64 = sqlx::query_scalar("SELECT id FROM manual_assets WHERE user_id=$1")
        .bind(user)
        .fetch_one(&pool)
        .await?;
    a.save(
        &format!("/my-assets/{asset}"),
        &[("value", "260000"), ("valued_on", "2026-11-02")],
    )
    .await;
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM asset_valuations WHERE manual_asset_id=$1"
        )
        .bind(asset)
        .fetch_one(&pool)
        .await?,
        2
    );
    a.save(
        "/recurring",
        &[
            ("name", "Internet"),
            ("amount", "50"),
            ("currency", "USD"),
            ("direction", "expense"),
            ("frequency", "monthly"),
            ("next_on", "2026-11-30"),
            ("month_end", "1"),
        ],
    )
    .await;
    let recurring: i64 = sqlx::query_scalar("SELECT id FROM recurring_items WHERE user_id=$1")
        .bind(user)
        .fetch_one(&pool)
        .await?;
    a.save(&format!("/recurring/{recurring}"), &[("status", "paused")])
        .await;
    a.save(
        "/transaction_rules",
        &[
            ("match_text", "Coffee"),
            ("description_operator", "contains"),
            ("category_id", &category.to_string()),
            ("kind", "regular"),
            ("tags", "coffee"),
        ],
    )
    .await;
    a.save(
        "/transactions",
        &[
            ("name", "Coffee shop"),
            ("amount", "5.25"),
            ("date", "2026-11-02"),
        ],
    )
    .await;
    let coffee: Value = sqlx::query_scalar(
        "SELECT to_jsonb(t) FROM transactions t WHERE user_id=$1 AND name='Coffee shop'",
    )
    .bind(user)
    .fetch_one(&pool)
    .await?;
    assert_eq!(coffee.id("category_id"), category);
    assert_eq!(coffee.s("tags"), "coffee");
    // CSV previews are committed atomically, with reimports skipped.
    let mut conn = pool.acquire().await?;
    let batch = db::save(
        &mut conn,
        "import_batches",
        user,
        None,
        json!({"csv_text":include_str!("fixtures/mint.csv"),"settings":{"currency":"USD"}}),
    )
    .await?;
    drop(conn);
    a.save(
        &format!("/imports/{}", batch.id("id")),
        &[
            ("currency", "USD"),
            ("commit_import", "1"),
            ("profile_name", "Mint"),
        ],
    )
    .await;
    let mut conn = pool.acquire().await?;
    let l = Ledger::load(&mut conn, user).await?;
    let preview = imports::csv_preview(
        &json!({"csv_text":include_str!("fixtures/mint.csv"),"settings":{"currency":"USD"}}),
        &l,
    )?;
    assert!(
        preview["rows"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r.flag("existing"))
    );
    drop(conn);
    // Workbook import preserves source categories, latest daily observations and account overrides.
    let workbook = imports::read_tiller(include_bytes!("fixtures/tiller.xlsx").to_vec())?;
    let mut conn = pool.acquire().await?;
    let batch=db::save(&mut conn,"import_batches",user,None,json!({"csv_text":workbook.to_string(),"settings":{"format":"tiller","currency":"USD","include_duplicates":"1"}})).await?;
    drop(conn);
    a.get(&format!("/imports/{}", batch.id("id"))).await;
    a.save(
        &format!("/imports/{}", batch.id("id")),
        &[
            ("currency", "USD"),
            ("include_duplicates", "1"),
            ("commit_import", "1"),
        ],
    )
    .await;
    a.get(&format!("/imports/{}", batch.id("id"))).await;
    let mut conn = pool.acquire().await?;
    let l = Ledger::load(&mut conn, user).await?;
    let checking = l
        .accounts
        .iter()
        .find(|r| r.s("import_source_key") == "tiller:checking")
        .unwrap();
    assert_eq!(checking.dec("current_balance"), Decimal::from(150));
    let snapshot = l
        .snapshots
        .iter()
        .find(|s| s.id("account_id") == checking.id("id") && s.s("as_of_date") == "2026-01-02")
        .unwrap();
    assert_eq!(snapshot.dec("current_balance"), Decimal::from(110));
    let debt = l
        .accounts
        .iter()
        .find(|r| r.s("import_source_key") == "tiller:debt")
        .unwrap();
    assert!(debt.flag("net_worth_excluded"));
    assert_eq!(debt.s("account_type"), "loan");
    let batch=db::save(&mut conn,"import_batches",user,None,json!({"csv_text":workbook.to_string(),"settings":{"format":"tiller","currency":"USD","include_duplicates":"1"}})).await?;
    let preview = imports::tiller_preview(&mut conn, &batch, &l).await?;
    assert_eq!(preview["summary"].id("already_imported"), 4);
    assert_eq!(preview["errors"], json!([]));
    drop(conn);
    a.save(
        &format!("/imports/{}", batch.id("id")),
        &[("currency", "USD"), ("commit_import", "1")],
    )
    .await;
    // Plaid upserts preserve manual choices, tracking and splits when amounts change.
    let mut conn = pool.acquire().await?;
    let institution:i64=sqlx::query_scalar("INSERT INTO institutions(name,plaid_institution_id) VALUES('Test Bank','test') RETURNING id").fetch_one(&mut *conn).await?;
    let item=db::save(&mut conn,"plaid_items",user,None,json!({"institution_id":institution,"plaid_item_id":"item-test","access_token":plaid::encrypt(&[42;32],"test-token","worth:plaid:1:item-test")?,"status":"active"})).await?;
    plaid::upsert_accounts(&mut conn,user,item.id("id"),&[json!({"account_id":"bank-checking","name":"Checking","type":"depository","balances":{"current":"500","iso_currency_code":"USD"}})]).await?;
    let account: i64 = sqlx::query_scalar(
        "SELECT id FROM accounts WHERE user_id=$1 AND plaid_account_id='bank-checking'",
    )
    .bind(user)
    .fetch_one(&mut *conn)
    .await?;
    plaid::upsert_transactions(&mut conn,user,item.id("id"),&[json!({"transaction_id":"pending1","account_id":"bank-checking","name":"Test bank purchase","date":"2026-11-01","amount":"20","iso_currency_code":"USD","pending":true})]).await?;
    let bank_id: i64 = sqlx::query_scalar(
        "SELECT id FROM transactions WHERE user_id=$1 AND plaid_transaction_id='pending1'",
    )
    .bind(user)
    .fetch_one(&mut *conn)
    .await?;
    drop(conn);
    a.save(
        &format!("/transactions/{bank_id}/split"),
        &[("splits[0][amount]", "10"), ("splits[1][amount]", "10")],
    )
    .await;
    let mut conn = pool.acquire().await?;
    plaid::upsert_transactions(&mut conn,user,item.id("id"),&[json!({"transaction_id":"posted1","pending_transaction_id":"pending1","account_id":"bank-checking","name":"Test bank purchase","date":"2026-11-02","amount":"21","iso_currency_code":"USD","pending":false})]).await?;
    let updated = db::owned(&mut conn, "transactions", user, bank_id).await?;
    assert!(updated.flag("split_needs_review"));
    assert_eq!(updated.s("plaid_transaction_id"), "posted1");
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM transactions WHERE parent_id=$1")
            .bind(bank_id)
            .fetch_one(&mut *conn)
            .await?,
        2
    );
    drop(conn);
    a.save(&format!("/plaid_items/{}/tracking", item.id("id")), &[])
        .await;
    let mut conn = pool.acquire().await?;
    let account_row = db::owned(&mut conn, "accounts", user, account).await?;
    assert!(!account_row.flag("tracked"));
    plaid::upsert_accounts(&mut conn,user,item.id("id"),&[json!({"account_id":"bank-checking","name":"Checking renamed","type":"depository","balances":{"current":"600","iso_currency_code":"USD"}})]).await?;
    assert!(
        !db::owned(&mut conn, "accounts", user, account)
            .await?
            .flag("tracked")
    );
    drop(conn);
    assert!(plaid::enqueue(&state, user, item.id("id")).await?);
    assert!(!plaid::enqueue(&state, user, item.id("id")).await?);
    assert!(plaid::enqueue(&state, second, item.id("id")).await.is_err());
    for path in [
        "/",
        "/transactions",
        "/transactions/new",
        "/transactions/duplicates",
        "/categories",
        "/transaction_rules",
        "/budgets",
        "/my-assets",
        "/recurring",
        "/trends",
        "/imports/new",
        "/bank_connections/new",
        "/password/edit",
    ] {
        let html = a.get(path).await;
        assert!(html.contains("Worth"), "{path}");
        assert!(!html.contains("test-token"));
        assert!(!html.contains("password_digest"));
    }
    let csv = a.get("/transactions.csv").await;
    assert!(csv.starts_with("Date,Description,Amount"));
    assert!(csv.contains("Coffee shop"));
    let (status, _, _) = a
        .request("GET", "/transactions?from=bad-date", String::new(), "")
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    a.save(
        "/password",
        &[
            ("current_password", "correct-horse-battery"),
            ("password", "different-correct-horse"),
            ("password_confirmation", "different-correct-horse"),
        ],
    )
    .await;
    let mut stale = Browser {
        app: app.clone(),
        cookie: old_cookie,
        csrf: String::new(),
    };
    let (status, _, location) = stale
        .request("GET", "/transactions", String::new(), "")
        .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(location, "/sign_in");
    // This test only ever destroys the randomly named database it created above.
    drop(a);
    drop(b);
    drop(stale);
    drop(app);
    drop(state);
    pool.close().await;
    sqlx::query(&format!("DROP DATABASE {name} WITH (FORCE)"))
        .execute(&admin)
        .await?;
    admin.close().await;
    Ok(())
}
use rust_decimal::Decimal;
