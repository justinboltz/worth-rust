use rust_decimal::Decimal;
use serde_json::{Value, json};
use worth::{
    domain::{self, Ledger, Record},
    imports, plaid,
};
fn t(id: i64, name: &str, amount: &str, date: &str) -> Value {
    json!({"id":id,"user_id":1,"name":name,"amount":amount,"date":date,"iso_currency_code":"USD","source":"manual","created_at":"2026-01-01T00:00:00"})
}
#[test]
fn precise_money_and_import_signs() {
    assert_eq!(
        domain::amount("0.1").unwrap() + domain::amount("0.2").unwrap(),
        domain::amount("0.3").unwrap()
    );
    assert!(domain::amount("0.00001").is_err());
    assert!(domain::amount("NaN").is_err());
    assert_eq!(
        imports::money("($1,234.56)").unwrap(),
        domain::amount("-1234.56").unwrap()
    );
    let l = Ledger::default();
    let b = json!({"csv_text":include_str!("fixtures/mint.csv"),"settings":{}});
    let p = imports::csv_preview(&b, &l).unwrap();
    assert_eq!(p["rows"][0].dec("amount"), domain::amount("25.50").unwrap());
    assert_eq!(p["rows"][1].dec("amount"), Decimal::from(-2500));
}
#[test]
fn splits_inherit_parent_exclusions() {
    let mut parent = t(1, "Purchase", "100", "2026-11-01");
    parent["is_split"] = json!(true);
    let mut child = t(2, "Food", "100", "2026-11-01");
    child["parent_id"] = json!(1);
    let mut l = Ledger {
        transactions: vec![parent.clone(), child.clone()],
        ..Default::default()
    };
    assert!(l.reportable(&child));
    assert!(!l.reportable(&parent));
    for flag in domain::FLAGS {
        l.transactions[0][flag] = json!(true);
        assert!(!l.reportable(&child), "{flag}");
        l.transactions[0][flag] = json!(false)
    }
    l.transactions[0]["account_id"] = json!(5);
    l.accounts = vec![json!({"id":5,"tracked":false})];
    assert!(!l.reportable(&child));
}
#[test]
fn rules_combine_conditions_and_preserve_manual_categories() {
    let rule = json!({"user_id":1,"match_text":"Market","description_operator":"starts_with","account_id":2,"category_id":7,"kind":"transfer","tags":"food, food"});
    let mut row = t(1, "Market local", "25", "2026-11-01");
    row["account_id"] = json!(3);
    assert!(!domain::rule_matches(&rule, &row));
    row["account_id"] = json!(2);
    assert!(domain::rule_matches(&rule, &row));
    row["category_id"] = json!(9);
    row["category_locked"] = json!(true);
    row["category_origin"] = json!("manual");
    row["classification_origin"] = json!("manual");
    domain::apply_rule(&rule, &mut row);
    assert_eq!(row.id("category_id"), 9);
    assert!(!row.flag("transfer"));
    assert_eq!(row.s("tags"), "food");
}
#[test]
fn recurring_month_end_and_original_day_do_not_drift() {
    let r = json!({"next_on":"2024-01-31","frequency":"monthly","status":"active"});
    assert_eq!(domain::occurrence(&r, 1).to_string(), "2024-02-29");
    assert_eq!(domain::occurrence(&r, 2).to_string(), "2024-03-31");
    let r =
        json!({"next_on":"2026-02-28","frequency":"monthly","month_end":true,"status":"active"});
    assert_eq!(domain::occurrence(&r, 1).to_string(), "2026-03-31");
    let r = json!({"next_on":"2026-11-01","frequency":"weekly","status":"active"});
    assert_eq!(
        domain::occurrences(
            &r,
            domain::date("2026-11-01").unwrap(),
            domain::date("2026-11-30").unwrap()
        )
        .len(),
        5
    );
}
#[test]
fn matching_amounts_alone_do_not_hide_purchases() {
    let mut a = t(1, "Groceries", "50", "2026-11-01");
    a["account_id"] = json!(1);
    let mut b = t(2, "Salary", "-50", "2026-11-01");
    b["account_id"] = json!(2);
    let l = Ledger {
        transactions: vec![a, b],
        accounts: vec![
            json!({"id":1,"name":"Checking","account_type":"depository"}),
            json!({"id":2,"name":"Savings","account_type":"depository"}),
        ],
        ..Default::default()
    };
    let rows = domain::classify(&l);
    assert!(
        rows.iter()
            .all(|t| !t.flag("transfer") && !t.flag("card_payment"))
    );
}
#[test]
fn corroborated_transfers_pair_and_ambiguous_transfers_remain_suggestions() {
    let mut a = t(1, "Transfer", "50", "2026-11-01");
    a["account_id"] = json!(1);
    let mut b = t(2, "Transfer", "-50", "2026-11-02");
    b["account_id"] = json!(2);
    let mut l = Ledger {
        transactions: vec![a, b.clone()],
        accounts: vec![
            json!({"id":1,"name":"Checking"}),
            json!({"id":2,"name":"Savings"}),
        ],
        ..Default::default()
    };
    let rows = domain::classify(&l);
    assert!(rows.iter().all(|t| t.flag("transfer")));
    b["id"] = json!(3);
    l.transactions.push(b);
    let rows = domain::classify(&l);
    assert!(
        rows.iter()
            .all(|t| !t.flag("transfer") && t.s("suggested_classification") == "transfer")
    );
}
#[test]
fn manual_regular_choice_is_protected() {
    let mut row = t(1, "Credit card payment", "50", "2026-11-01");
    row["classification_override"] = json!(true);
    row["classification_origin"] = json!("manual");
    let l = Ledger {
        transactions: vec![row],
        ..Default::default()
    };
    assert!(domain::classify(&l).is_empty());
}
#[test]
fn expenses_inherit_explicit_parent_then_category() {
    let mut parent = t(1, "Purchase", "100", "2026-11-01");
    parent["expense_type"] = json!("fixed");
    let mut child = t(2, "Food", "100", "2026-11-01");
    child["parent_id"] = json!(1);
    child["category_id"] = json!(10);
    let mut l = Ledger {
        transactions: vec![parent, child.clone()],
        categories: vec![
            json!({"id":10,"name":"Groceries","system":true,"parent_id":9}),
            json!({"id":9,"name":"Food & Dining","system":true}),
        ],
        ..Default::default()
    };
    assert_eq!(l.expense(&child), "fixed");
    child["expense_type"] = json!("variable");
    assert_eq!(l.expense(&child), "variable");
    l.expenses
        .push(json!({"category_id":9,"expense_type":"fixed"}));
    assert_eq!(l.category_expense(10), "fixed");
}
#[test]
fn merchant_history_requires_two_unanimous_manual_choices() {
    let mut a = t(1, "Market", "25", "2026-11-01");
    a["category_id"] = json!(5);
    a["category_locked"] = json!(true);
    a["category_origin"] = json!("manual");
    let mut b = a.clone();
    b["id"] = json!(2);
    let c = t(3, "Market", "20", "2026-11-03");
    let l = Ledger {
        user: 1,
        transactions: vec![a, b, c],
        categories: vec![json!({"id":5,"name":"Groceries","user_id":1})],
        ..Default::default()
    };
    let rows = domain::categorize(&l);
    assert_eq!(rows[0].id("category_id"), 5);
    assert_eq!(rows[0].s("category_origin"), "merchant_history");
}
#[test]
fn rollover_carries_overspending_and_includes_children() {
    let mut row = t(1, "Food", "120", "2026-10-15");
    row["category_id"] = json!(2);
    let l = Ledger {
        transactions: vec![row],
        categories: vec![
            json!({"id":1,"name":"Food"}),
            json!({"id":2,"name":"Groceries","parent_id":1}),
        ],
        ..Default::default()
    };
    let budget = json!({"category_id":1,"amount":"100","currency":"USD","starts_on":"2026-10-01","rollover":true});
    let summary = l.budget_summary(&budget, domain::date("2026-11-01").unwrap());
    assert_eq!(summary.dec("available"), Decimal::from(80));
}
#[test]
fn net_worth_uses_latest_observations_and_separates_currencies() {
    let l = Ledger {
        accounts: vec![
            json!({"id":1,"account_type":"credit","tracked":true,"iso_currency_code":"USD"}),
            json!({"id":2,"account_type":"depository","tracked":true,"iso_currency_code":"USD"}),
        ],
        snapshots: vec![
            json!({"account_id":1,"as_of_date":"2026-10-01","current_balance":"100","iso_currency_code":"USD"}),
            json!({"account_id":2,"as_of_date":"2026-11-01","current_balance":"500","iso_currency_code":"USD"}),
        ],
        ..Default::default()
    };
    let points = l.net_worth(
        "USD",
        domain::date("2026-11-01").unwrap(),
        domain::date("2026-11-30").unwrap(),
    );
    assert_eq!(points[0].dec("value"), Decimal::from(400));
    assert_eq!(points[0].id("known_accounts"), 2);
    assert!(
        l.net_worth(
            "EUR",
            domain::date("2026-11-01").unwrap(),
            domain::date("2026-11-30").unwrap()
        )
        .is_empty()
    );
}
#[test]
fn encrypted_tokens_bind_to_owner_and_detect_tampering() {
    let key = [7; 32];
    let encrypted = plaid::encrypt(&key, "test-bank-token", "worth:plaid:1:item").unwrap();
    assert_eq!(
        plaid::decrypt(&key, &encrypted, "worth:plaid:1:item").unwrap(),
        "test-bank-token"
    );
    assert!(plaid::decrypt(&key, &encrypted, "worth:plaid:2:item").is_err());
    assert!(plaid::decrypt(&[8; 32], &encrypted, "worth:plaid:1:item").is_err());
    assert!(!encrypted.contains("test-bank-token"));
}
#[test]
fn tiller_reads_cached_cells_overrides_and_stable_ids() {
    let data = imports::read_tiller(include_bytes!("fixtures/tiller.xlsx").to_vec()).unwrap();
    assert_eq!(data["errors"], json!([]));
    assert_eq!(data["transactions"].as_array().unwrap().len(), 4);
    assert_eq!(
        data["transactions"][0].dec("amount"),
        domain::amount("25.5").unwrap()
    );
    assert_eq!(data["transactions"][1].dec("amount"), Decimal::from(-2500));
    assert!(data["transactions"][2].flag("transfer"));
    assert!(data["transactions"][2].flag("hidden"));
    let debt = data["accounts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a.s("key") == "debt")
        .unwrap();
    assert_eq!(debt.s("class"), "Liability");
    assert!(debt.flag("hidden"));
    assert_eq!(data["balances"].as_array().unwrap().len(), 4);
    let again = imports::read_tiller(include_bytes!("fixtures/tiller.xlsx").to_vec()).unwrap();
    assert_eq!(data, again);
}
#[test]
fn templates_compile() {
    let mut env = minijinja::Environment::new();
    env.set_loader(minijinja::path_loader(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/templates"
    )));
    for path in std::fs::read_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/templates")).unwrap() {
        let path = path.unwrap().path();
        let name = path.file_name().unwrap().to_str().unwrap();
        env.get_template(name)
            .unwrap_or_else(|e| panic!("{name}: {e:#}"));
    }
}

#[test]
fn tiller_matches_rails_reference_identities_and_values() {
    let actual = imports::read_tiller(include_bytes!("fixtures/tiller.xlsx").to_vec()).unwrap();
    let expected: Value =
        serde_json::from_str(include_str!("fixtures/tiller-rails-expected.json")).unwrap();
    for key in ["transactions", "balances", "categories", "rules", "errors"] {
        assert_eq!(actual[key], expected[key], "{key}");
    }
    let mut actual_accounts = actual["accounts"].as_array().unwrap().clone();
    let mut expected_accounts = expected["accounts"].as_array().unwrap().clone();
    actual_accounts.sort_by(|a, b| a.s("key").cmp(b.s("key")));
    expected_accounts.sort_by(|a, b| a.s("key").cmp(b.s("key")));
    assert_eq!(actual_accounts, expected_accounts);
}
#[test]
fn duplicate_review_matches_bank_posting_window() {
    let mut bank = t(1, "Fuel Stop", "25", "2026-11-15");
    bank["source"] = json!("plaid");
    let mut imported = t(2, "  FUEL   STOP ", "25", "2026-11-01");
    imported["source"] = json!("import");
    let mut old = imported.clone();
    old["id"] = json!(3);
    old["date"] = json!("2026-10-31");
    let l = Ledger {
        transactions: vec![bank, imported, old],
        ..Default::default()
    };
    assert_eq!(worth::actions::duplicate_groups(&l), vec![vec![1, 2]]);
}
