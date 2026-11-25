use anyhow::{Result, bail};
use serde_json::Value;
use std::io::Read;
use worth::{db, domain::Record, plaid, web::AppState};
#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    let state = AppState::from_env().await?;
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input)?;
    let mut export: Value = serde_json::from_str(&input)?;
    if export["version"] != 1 {
        bail!("Unsupported Rails export")
    }
    let tables = export["tables"]
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("Missing export tables"))?;
    let order = [
        "users",
        "institutions",
        "plaid_items",
        "accounts",
        "categories",
        "category_expense_types",
        "manual_assets",
        "asset_valuations",
        "balance_snapshots",
        "budgets",
        "import_profiles",
        "import_batches",
        "recurring_dismissals",
        "recurring_items",
        "transaction_rules",
        "transactions",
    ];
    let mut tx = state.pool.begin().await?;
    for table in order {
        db::columns(table)?;
        let predicate = if table == "categories" {
            "WHERE user_id IS NOT NULL"
        } else {
            ""
        };
        let count: i64 = sqlx::query_scalar(&format!("SELECT count(*) FROM {table} {predicate}"))
            .fetch_one(&mut *tx)
            .await?;
        if count != 0 {
            bail!(
                "Destination already contains {table}. Use a fresh worth_rust_* database; imports never overwrite an existing ledger."
            )
        }
    }
    sqlx::query("DELETE FROM categories WHERE parent_id IS NOT NULL")
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM categories")
        .execute(&mut *tx)
        .await?;
    let mut counts = vec![];
    for table in order {
        let rows = tables
            .get_mut(table)
            .and_then(|v| v.as_array_mut())
            .ok_or_else(|| anyhow::anyhow!("Export is missing {table}"))?;
        // All foreign keys are satisfied by table order. Self references are inserted
        // in one SQL statement so a later-ID parent or matched transaction is valid.
        if table == "plaid_items" {
            for row in rows.iter_mut() {
                row["access_token"] = Value::String(plaid::encrypt(
                    &state.encryption_key,
                    row.s("access_token"),
                    &format!(
                        "worth:plaid:{}:{}",
                        row.id("user_id"),
                        row.s("plaid_item_id")
                    ),
                )?);
                row["sync_requested_at"] = Value::Null;
                row["sync_request_token"] = Value::Null;
            }
        }
        let columns = db::columns(table)?
            .iter()
            .map(|k| format!("\"{k}\""))
            .collect::<Vec<_>>()
            .join(",");
        sqlx::query(&format!("INSERT INTO {table} ({columns}) SELECT {columns} FROM jsonb_populate_recordset(NULL::{table},$1)")).bind(serde_json::to_value(&*rows)?).execute(&mut *tx).await?;
        sqlx::query(&format!("SELECT setval(pg_get_serial_sequence('{table}','id'),COALESCE((SELECT MAX(id) FROM {table}),1),(SELECT COUNT(*)>0 FROM {table}))")).execute(&mut *tx).await?;
        counts.push((table, rows.len()));
    }
    if let Some(users) = export["verification"].as_array() {
        use rust_decimal::Decimal;
        use std::collections::BTreeMap;
        for expected in users {
            let ledger = worth::domain::Ledger::load(&mut tx, expected.id("user_id")).await?;
            let mut actual = BTreeMap::<(String, String), (i64, Decimal, Decimal)>::new();
            for row in ledger
                .transactions
                .iter()
                .filter(|row| ledger.reportable(row))
            {
                let entry = actual
                    .entry((
                        row.s("date")[..7].to_string(),
                        row.s("iso_currency_code").to_string(),
                    ))
                    .or_default();
                entry.0 += 1;
                if row.dec("amount") > Decimal::ZERO {
                    entry.1 += row.dec("amount")
                } else {
                    entry.2 -= row.dec("amount")
                }
            }
            let reference = expected["months"]
                .as_array()
                .ok_or_else(|| anyhow::anyhow!("Missing comparison totals"))?
                .iter()
                .map(|row| {
                    (
                        (row.s("month").to_string(), row.s("currency").to_string()),
                        (row.id("count"), row.dec("spending"), row.dec("income")),
                    )
                })
                .collect::<BTreeMap<_, _>>();
            if actual != reference {
                bail!(
                    "Monthly report comparison failed for user {}; destination import rolled back",
                    expected.id("user_id")
                )
            }
            println!(
                "Verified {} monthly currency totals for user {}",
                actual.len(),
                expected.id("user_id")
            );
        }
    }
    tx.commit().await?;
    if let Some(config) = export["plaid_config"].as_object() {
        use std::io::Write;
        let mut options = std::fs::OpenOptions::new();
        options.create(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(".env")?;
        for key in ["PLAID_CLIENT_ID", "PLAID_SECRET", "PLAID_ENV"] {
            if let Some(value) = config.get(key).and_then(|v| v.as_str()) {
                if value.contains(['\n', '\r', '\'']) {
                    bail!("Unsupported configuration value")
                };
                writeln!(file, "{key}='{value}'")?;
            }
        }
    }
    for (table, count) in counts {
        println!("{table}: {count}")
    }
    println!(
        "Rails data copied. IDs and password hashes preserved; bank tokens re-encrypted. No bank refresh was requested."
    );
    Ok(())
}
