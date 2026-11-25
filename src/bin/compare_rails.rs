use anyhow::{Result, bail};
use serde_json::Value;
use std::io::Read;
use worth::{
    domain::{self, Ledger, Record},
    web::AppState,
};
#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    let state = AppState::from_env().await?;
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input)?;
    let reference: Vec<Value> = serde_json::from_str(&input)?;
    let mut conn = state.pool.acquire().await?;
    let mut count = 0;
    for expected in reference {
        let ledger = Ledger::load(&mut conn, expected.id("user_id")).await?;
        let report = ledger.report(
            domain::date("1900-01-01")?,
            domain::today(),
            expected.s("currency"),
        );
        for key in ["spending", "income"] {
            if report.dec(key) != expected.dec(key) {
                bail!(
                    "{key} differs for user {}, {}",
                    expected.id("user_id"),
                    expected.s("currency")
                )
            }
        }
        let history = ledger.net_worth(
            expected.s("currency"),
            domain::date("1900-01-01")?,
            domain::today(),
        );
        let expected_history = expected["history"].as_array().unwrap();
        if history.len() != expected_history.len() {
            bail!("Net-worth observation count differs")
        };
        for (actual, expected) in history.iter().zip(expected_history) {
            for key in [
                "date",
                "known_accounts",
                "total_accounts",
                "known_assets",
                "total_assets",
            ] {
                if actual[key] != expected[key] {
                    bail!("Net-worth {key} differs on {}", actual.s("date"))
                }
            }
            if actual.dec("value") != expected.dec("value") {
                bail!("Net-worth value differs on {}", actual.s("date"))
            }
        }
        count += history.len();
        println!(
            "Matched cash flow and {} net-worth observations for user {}, {}",
            history.len(),
            expected.id("user_id"),
            expected.s("currency")
        );
    }
    println!("All comparisons passed ({count} net-worth observations). No data changed.");
    Ok(())
}
