use anyhow::{Result, bail};
use serde_json::{Value, json};
use sqlx::{PgConnection, PgPool};
use std::collections::HashMap;
use std::sync::LazyLock;

static SCHEMA: LazyLock<HashMap<String, Vec<String>>> =
    LazyLock::new(|| serde_json::from_str(include_str!("schema.json")).unwrap());
pub fn columns(table: &str) -> Result<&'static Vec<String>> {
    SCHEMA
        .get(table)
        .ok_or_else(|| anyhow::anyhow!("Unknown record type"))
}
pub async fn rows(conn: &mut PgConnection, table: &str, user: i64) -> Result<Vec<Value>> {
    columns(table)?;
    let predicate = if table == "categories" {
        "user_id = $1 OR system = true"
    } else {
        "user_id = $1"
    };
    Ok(sqlx::query_scalar::<_, Value>(&format!(
        "SELECT to_jsonb(t) FROM {table} t WHERE {predicate} ORDER BY id"
    ))
    .bind(user)
    .fetch_all(conn)
    .await?)
}
pub async fn owned(conn: &mut PgConnection, table: &str, user: i64, id: i64) -> Result<Value> {
    columns(table)?;
    let predicate = if table == "categories" {
        "(user_id = $1 OR system = true)"
    } else {
        "user_id = $1"
    };
    sqlx::query_scalar::<_, Value>(&format!(
        "SELECT to_jsonb(t) FROM {table} t WHERE {predicate} AND id = $2"
    ))
    .bind(user)
    .bind(id)
    .fetch_optional(conn)
    .await?
    .ok_or_else(|| anyhow::anyhow!("Record not found"))
}
/// Callers validate business rules; this layer rejects unknown fields and scopes every update.
pub async fn save(
    conn: &mut PgConnection,
    table: &str,
    user: i64,
    id: Option<i64>,
    mut attrs: Value,
) -> Result<Value> {
    let fields = columns(table)?;
    let map = attrs
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("Expected record"))?;
    map.remove("id");
    map.remove("created_at");
    map.remove("updated_at");
    if fields.iter().any(|s| s == "user_id") {
        map.insert("user_id".into(), json!(user));
    }
    for key in map.keys() {
        if !fields.contains(key) {
            bail!("Unsupported field: {key}");
        }
    }
    let keys: Vec<String> = map.keys().map(|k| format!("\"{k}\"")).collect();
    let query = if let Some(id) = id {
        if !fields.iter().any(|s| s == "user_id") {
            bail!("Unscoped update denied");
        }
        let assignments = keys
            .iter()
            .map(|k| format!("{k}=r.{k}"))
            .collect::<Vec<_>>()
            .join(",");
        format!(
            "UPDATE {table} t SET {assignments},updated_at=CURRENT_TIMESTAMP FROM jsonb_populate_record(NULL::{table},$1) r WHERE t.id={id} AND t.user_id={user} RETURNING to_jsonb(t)"
        )
    } else {
        format!(
            "INSERT INTO {table} ({}) SELECT {} FROM jsonb_populate_record(NULL::{table},$1) RETURNING to_jsonb({table})",
            keys.join(","),
            keys.join(",")
        )
    };
    sqlx::query_scalar::<_, Value>(&query)
        .bind(attrs)
        .fetch_optional(conn)
        .await?
        .ok_or_else(|| anyhow::anyhow!("Record not found"))
}
pub async fn delete(conn: &mut PgConnection, table: &str, user: i64, id: i64) -> Result<()> {
    columns(table)?;
    let changed = sqlx::query(&format!("DELETE FROM {table} WHERE user_id=$1 AND id=$2"))
        .bind(user)
        .bind(id)
        .execute(conn)
        .await?
        .rows_affected();
    if changed == 0 {
        bail!("Record not found");
    }
    Ok(())
}
pub async fn initialize(pool: &PgPool) -> Result<()> {
    sqlx::migrate!().run(pool).await?;
    let mut conn = pool.acquire().await?;
    let seeds = [
        (
            "Income",
            vec!["Paycheck", "Interest Income", "Other Income"],
        ),
        (
            "Bills & Utilities",
            vec![
                "Electricity",
                "Gas",
                "Internet",
                "Mobile Phone",
                "Rent",
                "Water",
            ],
        ),
        (
            "Food & Dining",
            vec!["Groceries", "Restaurants", "Coffee Shops"],
        ),
        (
            "Shopping",
            vec!["Clothing", "Electronics & Software", "Home Goods"],
        ),
        (
            "Auto & Transport",
            vec!["Gas & Fuel", "Parking", "Public Transportation"],
        ),
        ("Entertainment", vec![]),
        ("Health & Fitness", vec![]),
        ("Travel", vec![]),
        ("Fees & Charges", vec![]),
        ("Gifts & Donations", vec![]),
        ("Taxes", vec![]),
    ];
    for (name, children) in seeds {
        let parent = seed_category(&mut conn, name, None).await?;
        for child in children {
            seed_category(&mut conn, child, Some(parent)).await?;
        }
    }
    Ok(())
}
async fn seed_category(conn: &mut PgConnection, name: &str, parent: Option<i64>) -> Result<i64> {
    if let Some(id)=sqlx::query_scalar("SELECT id FROM categories WHERE system=true AND user_id IS NULL AND name=$1 AND parent_id IS NOT DISTINCT FROM $2 LIMIT 1").bind(name).bind(parent).fetch_optional(&mut *conn).await? {return Ok(id)}
    Ok(sqlx::query_scalar(
        "INSERT INTO categories(name,parent_id,system) VALUES($1,$2,true) RETURNING id",
    )
    .bind(name)
    .bind(parent)
    .fetch_one(conn)
    .await?)
}
