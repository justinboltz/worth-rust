use crate::{
    db,
    domain::{self, Ledger, Record},
    web::{self, AppState, Input},
};
use anyhow::{Result, bail};
use axum::response::Response;
use chrono::{Duration, NaiveDate};
use rust_decimal::{Decimal, prelude::ToPrimitive};
use serde_json::{Value, json};
use sqlx::PgConnection;
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    io::{Cursor, Read},
    str::FromStr,
};

pub fn rails_json(v: &Value) -> String {
    v.to_string()
        .replace('&', "\\u0026")
        .replace('<', "\\u003c")
        .replace('>', "\\u003e")
        .replace('\u{2028}', "\\u2028")
        .replace('\u{2029}', "\\u2029")
}
pub fn fixed(n: Decimal) -> String {
    let s = n.normalize().to_string();
    if s.contains('.') { s } else { format!("{s}.0") }
}
pub fn money(raw: &str) -> Result<Decimal> {
    let mut s = raw.replace([',', '$', ' '], "");
    if s.starts_with('(') && s.ends_with(')') {
        s = format!("-{}", &s[1..s.len() - 1])
    }
    domain::amount(&s)
}
fn hidden(s: &str) -> bool {
    ["hide", "hidden", "true", "yes", "1"].contains(&s.to_lowercase().as_str())
}
fn parse_date(s: &str, epoch: NaiveDate) -> Result<NaiveDate> {
    let date = if let Ok(n) = Decimal::from_str(s) {
        epoch
            + Duration::days(
                n.floor()
                    .to_i64()
                    .ok_or_else(|| anyhow::anyhow!("Invalid date"))?,
            )
    } else {
        NaiveDate::parse_from_str(s, "%m/%d/%Y")
            .or_else(|_| NaiveDate::parse_from_str(s, "%Y-%m-%d"))?
    };
    if date < domain::date("1900-01-01")? || date > domain::today() {
        bail!("Date must be between 1900 and today")
    }
    Ok(date)
}
struct Workbook {
    archive: zip::ZipArchive<Cursor<Vec<u8>>>,
    sheets: HashMap<String, String>,
    strings: Vec<String>,
    epoch: NaiveDate,
}
impl Workbook {
    fn xml(&mut self, path: &str) -> Result<String> {
        let mut entry = self.archive.by_name(path)?;
        if entry.size() > 60 * 1024 * 1024 {
            bail!("Workbook XML is too large")
        };
        let mut xml = String::new();
        entry
            .by_ref()
            .take(60 * 1024 * 1024 + 1)
            .read_to_string(&mut xml)?;
        if xml.len() > 60 * 1024 * 1024 || xml.contains("<!DOCTYPE") {
            bail!("Workbook XML is too large or contains a document type")
        }
        Ok(xml)
    }
    fn open(bytes: Vec<u8>) -> Result<Self> {
        if bytes.len() > 25 * 1024 * 1024 {
            bail!("Use an XLSX workbook under 25 MB")
        };
        let mut archive = zip::ZipArchive::new(Cursor::new(bytes))?;
        if archive.len() > 1000 {
            bail!("Workbook has too many archive entries")
        };
        let mut total = 0u64;
        for i in 0..archive.len() {
            total = total
                .checked_add(archive.by_index(i)?.size())
                .ok_or_else(|| anyhow::anyhow!("Workbook is too large"))?;
        }
        if total > 150 * 1024 * 1024 {
            bail!("Workbook is too large when expanded")
        }
        let mut book = Self {
            archive,
            sheets: HashMap::new(),
            strings: vec![],
            epoch: domain::date("1899-12-30")?,
        };
        let xml = book.xml("xl/workbook.xml")?;
        let doc = roxmltree::Document::parse(&xml)?;
        if doc
            .descendants()
            .find(|n| n.has_tag_name("workbookPr"))
            .and_then(|n| n.attribute("date1904"))
            .is_some_and(|s| s == "1" || s == "true")
        {
            book.epoch = domain::date("1904-01-01")?
        }
        let rels = book.xml("xl/_rels/workbook.xml.rels")?;
        let rels = roxmltree::Document::parse(&rels)?;
        for sheet in doc.descendants().filter(|n| n.has_tag_name("sheet")) {
            let id = sheet
                .attributes()
                .find(|a| a.name() == "id")
                .map(|a| a.value())
                .ok_or_else(|| anyhow::anyhow!("Worksheet reference missing"))?;
            let rel = rels
                .descendants()
                .find(|n| n.has_tag_name("Relationship") && n.attribute("Id") == Some(id))
                .ok_or_else(|| anyhow::anyhow!("Worksheet reference invalid"))?;
            if rel.attribute("TargetMode") == Some("External") {
                bail!("External worksheet links are unsupported")
            };
            let target = rel
                .attribute("Target")
                .ok_or_else(|| anyhow::anyhow!("Missing worksheet target"))?;
            let path = if target.starts_with('/') {
                target.trim_start_matches('/').to_string()
            } else {
                format!("xl/{target}")
            };
            let mut components = vec![];
            for part in path.split('/') {
                match part {
                    ".." => {
                        components.pop();
                    }
                    "." | "" => {}
                    _ => components.push(part),
                }
            }
            let path = components.join("/");
            if !path.starts_with("xl/") {
                bail!("Invalid worksheet path")
            };
            book.sheets
                .insert(sheet.attribute("name").unwrap_or("").to_string(), path);
        }
        if book.archive.by_name("xl/sharedStrings.xml").is_ok() {
            let xml = book.xml("xl/sharedStrings.xml")?;
            let doc = roxmltree::Document::parse(&xml)?;
            book.strings = doc
                .descendants()
                .filter(|n| n.has_tag_name("si"))
                .map(|n| {
                    n.descendants()
                        .filter(|n| n.has_tag_name("t"))
                        .filter_map(|n| n.text())
                        .collect::<String>()
                })
                .collect();
        }
        Ok(book)
    }
    fn table(&mut self, name: &str, required: &[&str]) -> Result<Vec<Value>> {
        let path = self
            .sheets
            .get(name)
            .ok_or_else(|| anyhow::anyhow!("Workbook is missing the {name} sheet"))?
            .clone();
        let xml = self.xml(&path)?;
        let doc = roxmltree::Document::parse(&xml)?;
        let rows = doc
            .descendants()
            .filter(|n| {
                n.has_tag_name("row") && n.parent().is_some_and(|p| p.has_tag_name("sheetData"))
            })
            .collect::<Vec<_>>();
        if rows.len() > 60_000 {
            bail!("{name} has too many rows")
        };
        let mut headers = vec![];
        let mut output = vec![];
        for row in rows {
            let mut cells = BTreeMap::new();
            for cell in row.children().filter(|n| n.has_tag_name("c")) {
                let address = cell.attribute("r").unwrap_or("");
                let column = address
                    .chars()
                    .take_while(|c| c.is_ascii_uppercase())
                    .collect::<String>();
                if column.is_empty() {
                    bail!("Invalid cell address in {name}")
                };
                let cached = cell
                    .children()
                    .find(|n| n.has_tag_name("v"))
                    .and_then(|n| n.text());
                if cell.children().any(|n| n.has_tag_name("f")) && cached.is_none() {
                    bail!(
                        "{name}!{address} has no saved formula result. Recalculate and export again."
                    )
                };
                let value = match cell.attribute("t") {
                    Some("e") => bail!("{name}!{address} contains a spreadsheet error"),
                    Some("s") => self
                        .strings
                        .get(cached.unwrap_or("").parse::<usize>()?)
                        .ok_or_else(|| anyhow::anyhow!("Invalid shared text"))?
                        .clone(),
                    Some("inlineStr") => cell
                        .descendants()
                        .filter(|n| n.has_tag_name("t"))
                        .filter_map(|n| n.text())
                        .collect::<String>(),
                    _ => cached.unwrap_or("").to_string(),
                };
                cells.insert(column, value.trim().to_string());
            }
            if cells.values().all(|v| v.is_empty()) {
                continue;
            }
            if headers.is_empty() {
                let mut counts = HashMap::<String, usize>::new();
                for (column, name) in cells {
                    if name.is_empty() {
                        continue;
                    }
                    let count = counts.entry(name.clone()).or_default();
                    *count += 1;
                    headers.push((
                        column,
                        if *count == 1 {
                            name
                        } else {
                            format!("{name} {count}")
                        },
                    ));
                }
                for r in required {
                    if !headers.iter().any(|(_, name)| name == r) {
                        bail!("{name} is missing column {r}")
                    }
                }
                continue;
            }
            let mut record =
                json!({"_row":row.attribute("r").unwrap_or("0").parse::<i64>().unwrap_or(0)});
            let mut nonblank = false;
            for (column, label) in &headers {
                let value = cells.get(column).cloned().unwrap_or_default();
                nonblank |= !value.is_empty();
                record[label] = json!(value)
            }
            if nonblank {
                output.push(record)
            }
        }
        if headers.is_empty() {
            bail!("{name} has no header row")
        };
        Ok(output)
    }
    fn optional(&mut self, name: &str, required: &[&str]) -> Result<Vec<Value>> {
        if self.sheets.contains_key(name) {
            self.table(name, required)
        } else {
            Ok(vec![])
        }
    }
}
fn source_account(
    accounts: &mut BTreeMap<String, Value>,
    id: &str,
    name: &str,
    institution: &str,
    mask: &str,
) -> String {
    let key = if id.is_empty() {
        format!(
            "legacy:{}",
            domain::hash(&rails_json(&json!([name, institution, mask])))
        )
    } else {
        id.to_string()
    };
    accounts.entry(key.clone()).or_insert_with(||json!({"key":key,"name":if name.is_empty(){"Imported account"}else{name},"institution":institution,"mask":mask,"hidden":false}));
    key
}
pub fn read_tiller(bytes: Vec<u8>) -> Result<Value> {
    let mut reader = Workbook::open(bytes)?;
    let mut accounts = BTreeMap::<String, Value>::new();
    let mut categories = vec![];
    let mut balances = vec![];
    let mut transactions = vec![];
    let mut errors = vec![];
    let mut warnings=vec!["Workbook charts are rebuilt from imported history in Worth. Choose the workbook currency; there is no exchange-rate conversion.".to_string()];
    let category_rows = reader.optional("Categories", &["Category", "Group", "Type"])?;
    for r in &category_rows {
        if !r.s("Category").is_empty() {
            categories.push(json!({"name":r.s("Category"),"group":r.s("Group"),"type":r.s("Type"),"hidden":hidden(r.s("Hide From Reports"))}))
        }
    }
    if categories
        .iter()
        .map(|c| c.s("name"))
        .collect::<HashSet<_>>()
        .len()
        != categories.len()
    {
        errors.push("Categories contains repeated category names".to_string())
    }
    let has_budgets = category_rows.iter().any(|r| {
        r.as_object().unwrap().iter().any(|(k, v)| {
            k.parse::<f64>().is_ok()
                && !v.as_str().unwrap_or("").is_empty()
                && money(v.as_str().unwrap_or("")).is_ok_and(|v| v != Decimal::ZERO)
        })
    });
    warnings.push(if has_budgets{"Monthly budget amounts need manual setup. Worth uses a single recurring amount per category."}else{"No nonzero monthly budgets were found. No budgets will be created."}.into());
    let rows = reader.optional("Accounts", &["Unique Account Identifier", "Account Id"])?;
    for r in &rows {
        if r.s("Account Id").is_empty() {
            continue;
        }
        let settings = rows
            .iter()
            .find(|o| o.s("Account") == r.s("Unique Account Identifier"))
            .unwrap_or(&Value::Null);
        let key = source_account(
            &mut accounts,
            r.s("Account Id"),
            if r.s("Account 2").is_empty() {
                r.s("Unique Account Identifier")
            } else {
                r.s("Account 2")
            },
            r.s("Institution"),
            r.s("Account #"),
        );
        let a = accounts.get_mut(&key).unwrap();
        for (dest, candidates) in [
            (
                "class",
                vec![
                    settings.s("Class Override"),
                    r.s("Class"),
                    r.s("Class (BH)"),
                ],
            ),
            ("group", vec![settings.s("Group"), r.s("Group 2")]),
        ] {
            a[dest] = json!(candidates.into_iter().find(|s| !s.is_empty()).unwrap_or(""))
        }
        a["type"] = json!(r.s("Type"));
        a["hidden"] = json!(hidden(if settings.s("Hide").is_empty() {
            r.s("Hide 2")
        } else {
            settings.s("Hide")
        }));
    }
    for r in reader.optional(
        "Balance History",
        &["Date", "Account", "Account ID", "Balance", "Class"],
    )? {
        let result: Result<Value> = (|| {
            if r.s("Account ID").is_empty() {
                bail!("Account ID is required")
            };
            let day = parse_date(r.s("Date"), reader.epoch)?;
            let mut stamp = Decimal::from_str(r.s("Date"))
                .unwrap_or_else(|_| Decimal::from((day - reader.epoch).num_days()));
            if stamp.fract() == Decimal::ZERO && !r.s("Time").is_empty() {
                stamp += Decimal::from_str(r.s("Time"))?.fract()
            };
            let key = source_account(
                &mut accounts,
                r.s("Account ID"),
                r.s("Account"),
                r.s("Institution"),
                r.s("Account #"),
            );
            let a = accounts.get_mut(&key).unwrap();
            if a.s("latest_at").is_empty() || stamp > a.dec("latest_at") {
                if a.s("class").is_empty() {
                    a["class"] = json!(r.s("Class"))
                }
                if a.s("type").is_empty() {
                    a["type"] = json!(r.s("Type"))
                }
                a["latest_at"] = json!(fixed(stamp));
            }
            if !["Asset", "Liability"].contains(&a.s("class")) {
                bail!("Class must be Asset or Liability")
            };
            Ok(
                json!({"account_key":key,"date":day,"timestamp":fixed(stamp),"balance":fixed(money(r.s("Balance"))?),"id":r.s("Balance ID"),"row":r["_row"]}),
            )
        })();
        match result {
            Ok(v) => balances.push(v),
            Err(e) => errors.push(format!("Balance History row {}: {e}", r.id("_row"))),
        }
    }
    let mut occurrences = HashMap::<String, usize>::new();
    let mut missing = 0;
    for r in reader.table(
        "Transactions",
        &["Date", "Description", "Amount", "Account"],
    )? {
        let result: Result<Value> = (|| {
            if r.s("Description").is_empty() {
                bail!("Description is required")
            };
            let day = parse_date(r.s("Date"), reader.epoch)?;
            let value = -money(r.s("Amount"))?;
            let key = source_account(
                &mut accounts,
                r.s("Account ID"),
                r.s("Account"),
                r.s("Institution"),
                r.s("Account #"),
            );
            if !r.s("Category").is_empty()
                && !categories.iter().any(|c| c.s("name") == r.s("Category"))
            {
                categories.push(json!({"name":r.s("Category"),"group":"","type":"","hidden":false}))
            }
            let category = categories.iter().find(|c| c.s("name") == r.s("Category"));
            let fingerprint = domain::hash(&rails_json(&json!([
                day,
                r.s("Description"),
                fixed(value),
                key
            ])));
            let id = r.s("Transaction ID");
            let identity = if id.is_empty() {
                missing += 1;
                format!("row:{fingerprint}")
            } else {
                format!(
                    "id:{id}:{}",
                    domain::hash(&rails_json(&json!([day, fixed(value), key])))
                )
            };
            let count = occurrences.entry(identity.clone()).or_default();
            *count += 1;
            let source_id = format!("tiller:{identity}:{count}");
            let mut metadata = r.clone();
            metadata.as_object_mut().unwrap().remove("_row");
            metadata.as_object_mut().unwrap().retain(|_, v| v != "");
            Ok(
                json!({"row":r["_row"],"date":day,"name":r.s("Description"),"amount":fixed(value),"account_key":key,"source_account_name":r.s("Account"),"category":r.s("Category"),"source_transaction_id":source_id,"original_description":r["Full Description"],"notes":r["Notes"],"tags":r["Tags"],"hidden":category.is_some_and(|c|c.flag("hidden")),"transfer":category.is_some_and(|c|c.s("type")=="Transfer"),"file_duplicate":*count>1,"metadata":metadata}),
            )
        })();
        match result {
            Ok(v) => transactions.push(v),
            Err(e) => errors.push(format!("Transactions row {}: {e}", r.id("_row"))),
        }
    }
    if missing > 0 {
        warnings.push(format!("{missing} older transactions have no Tiller ID. Unchanged rows will be recognized on reimport."))
    }
    let rules = reader.optional("AutoCat", &["Category", "Description Contains"])?;
    if !rules.is_empty() {
        warnings.push(format!(
            "{} AutoCat rules retained for reference; recreate desired rules in Worth.",
            rules.len()
        ))
    }
    Ok(
        json!({"transactions":transactions,"accounts":accounts.into_values().collect::<Vec<_>>(),"balances":balances,"categories":categories,"rules":rules,"errors":errors,"warnings":warnings}),
    )
}
pub fn csv_preview(batch: &Value, l: &Ledger) -> Result<Value> {
    let mut reader = csv::Reader::from_reader(
        batch
            .s("csv_text")
            .trim_start_matches('\u{feff}')
            .as_bytes(),
    );
    let headers = reader
        .headers()?
        .iter()
        .map(str::to_string)
        .collect::<Vec<_>>();
    if headers.is_empty()
        || headers.iter().any(|v| v.trim().is_empty())
        || headers.iter().collect::<HashSet<_>>().len() != headers.len()
    {
        bail!("CSV must have unique, nonblank headers")
    }
    let aliases = [
        ("date", vec!["date", "transaction date", "posted date"]),
        (
            "description",
            vec!["description", "merchant", "payee", "name"],
        ),
        ("amount", vec!["amount"]),
        ("category", vec!["category", "category name", "categories"]),
        ("account", vec!["account", "account name"]),
        ("type", vec!["transaction type", "type"]),
        ("notes", vec!["notes", "memo"]),
    ];
    let settings = &batch["settings"];
    let currency = if settings.s("currency").is_empty() {
        "USD"
    } else {
        settings.s("currency")
    };
    if !domain::CURRENCIES.contains(&currency) {
        bail!("Choose a supported currency")
    };
    let mut mapping = json!({});
    for (k, names) in aliases {
        mapping[k] = if settings["headers"].get(k).is_some() {
            settings["headers"][k].clone()
        } else {
            json!(
                headers
                    .iter()
                    .find(|h| names.contains(&h.trim().to_lowercase().as_str()))
            )
        };
    }
    let mut errors = vec![];
    for k in ["date", "description", "amount"] {
        if !headers.iter().any(|h| h == mapping.s(k)) {
            errors.push(format!("Map the required {k} column"))
        }
    }
    if !errors.is_empty() {
        return Ok(
            json!({"headers":headers,"mapping":mapping,"rows":[],"errors":errors,"source_categories":[]}),
        );
    }
    let mut rows = vec![];
    let mut occurrences = HashMap::<String, usize>::new();
    let mut source_categories = HashSet::new();
    for (index, r) in reader.records().enumerate() {
        let r = r?;
        if r.iter().all(|s| s.trim().is_empty()) {
            continue;
        }
        let result: Result<Value> = (|| {
            let value = |k: &str| {
                headers
                    .iter()
                    .position(|h| h == mapping.s(k))
                    .and_then(|i| r.get(i))
                    .unwrap_or("")
                    .trim()
            };
            let day = NaiveDate::parse_from_str(value("date"), "%m/%d/%Y")
                .or_else(|_| NaiveDate::parse_from_str(value("date"), "%Y-%m-%d"))?;
            let name = value("description");
            if name.is_empty() {
                bail!("Description is required")
            };
            let mut amount = money(value("amount"))?;
            match value("type").to_lowercase().as_str() {
                "credit" => amount = -amount.abs(),
                "debit" => amount = amount.abs(),
                _ => {}
            }
            if settings.s("reverse_sign") == "1" {
                amount = -amount
            }
            let identity = format!(
                "{day}|{name}|{}|{}|{currency}",
                fixed(amount),
                value("account")
            );
            let count = occurrences.entry(identity.clone()).or_default();
            *count += 1;
            let source_id = domain::hash(&format!("{identity}|{count}"));
            let existing = l
                .transactions
                .iter()
                .any(|t| t.s("source") == "import" && t.s("source_transaction_id") == source_id);
            let duplicate = *count > 1
                || l.transactions.iter().any(|t| {
                    t.id("parent_id") == 0
                        && t.s("date") == day.to_string()
                        && t.dec("amount") == amount
                        && t.s("iso_currency_code") == currency
                });
            source_categories.insert(value("category").to_string());
            Ok(
                json!({"number":index+2,"date":day,"name":name,"amount":fixed(amount),"source_account_name":if value("account").is_empty(){None}else{Some(value("account"))},"source_category":value("category"),"notes":value("notes"),"iso_currency_code":currency,"source_transaction_id":source_id,"existing":existing,"duplicate":duplicate}),
            )
        })();
        match result {
            Ok(row) => rows.push(row),
            Err(e) => {
                errors.push(format!("Row {}: {e}", index + 2));
                rows.push(json!({"number":index+2,"error":e.to_string()}));
            }
        }
    }
    let mut source_categories = source_categories
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>();
    source_categories.sort();
    Ok(
        json!({"headers":headers,"mapping":mapping,"rows":rows,"errors":errors,"source_categories":source_categories}),
    )
}
fn array(v: &Value) -> Vec<Value> {
    v.as_array().cloned().unwrap_or_default()
}
pub async fn tiller_preview(conn: &mut PgConnection, batch: &Value, l: &Ledger) -> Result<Value> {
    let data: Value = serde_json::from_str(batch.s("csv_text"))?;
    let settings = &batch["settings"];
    let currency = if settings.s("currency").is_empty() {
        "USD"
    } else {
        settings.s("currency")
    };
    if !domain::CURRENCIES.contains(&currency) {
        bail!("Choose a supported currency")
    }
    let mut previous = db::rows(conn, "import_batches", l.user).await?;
    previous.retain(|b| !b["imported_at"].is_null() && b["settings"].s("format") == "tiller");
    previous.sort_by(|a, b| {
        a.s("imported_at")
            .cmp(b.s("imported_at"))
            .then(a.id("id").cmp(&b.id("id")))
    });
    let mut saved = json!({});
    let mut category_mapping = json!({});
    for b in &previous {
        for (k, v) in b["settings"]["accounts"].as_object().into_iter().flatten() {
            if v != "new" {
                saved[k] = v.clone()
            }
        }
        for (k, v) in b["settings"]["manual_asset_mapping"]
            .as_object()
            .into_iter()
            .flatten()
        {
            saved[k] = json!(format!(
                "asset:{}",
                v.as_i64()
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| v.as_str().unwrap_or("").to_string())
            ))
        }
        for (k, v) in b["settings"]["category_mapping"]
            .as_object()
            .into_iter()
            .flatten()
        {
            category_mapping[k] = v.clone()
        }
    }
    for t in &l.transactions {
        let name = t["raw_payload"].s("import_category");
        if t.s("source") == "import"
            && t.s("category_origin") == "import"
            && t.id("category_id") != 0
            && !name.is_empty()
            && category_mapping[name].is_null()
        {
            let ids = l
                .transactions
                .iter()
                .filter(|r| {
                    r.s("source") == "import"
                        && r.s("category_origin") == "import"
                        && r["raw_payload"].s("import_category") == name
                        && r.id("category_id") != 0
                })
                .map(|r| r.id("category_id"))
                .collect::<HashSet<_>>();
            if ids.len() == 1 {
                category_mapping[name] = json!(t.id("category_id").to_string())
            }
        }
    }
    let institutions:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_object('account_id',a.id,'institution',i.name) FROM accounts a JOIN plaid_items p ON p.id=a.plaid_item_id JOIN institutions i ON i.id=p.institution_id WHERE a.user_id=$1").bind(l.user).fetch_all(&mut *conn).await?;
    let mut sources = array(&data["accounts"]);
    let mut errors = array(&data["errors"]);
    let mut selections = json!({});
    for source in &mut sources {
        let key = source.s("key").to_string();
        let known = l
            .accounts
            .iter()
            .find(|a| a.s("import_source_key") == format!("tiller:{key}"))
            .map(|a| a.id("id").to_string())
            .or_else(|| {
                let s = saved.s(&key);
                if s.starts_with("asset:")
                    && l.assets
                        .iter()
                        .any(|a| s == format!("asset:{}", a.id("id")))
                {
                    Some(s.to_string())
                } else {
                    None
                }
            });
        let old = saved.s(&key);
        let available_old = old == "transactions_only"
            || l.accounts.iter().any(|a| a.id("id").to_string() == old)
            || l.assets
                .iter()
                .any(|a| format!("asset:{}", a.id("id")) == old);
        let mut suggestion = None;
        if key.starts_with("manual:") && source.s("class") == "Asset" && !source.flag("hidden") {
            let matches = l
                .assets
                .iter()
                .filter(|a| {
                    a.s("currency") == currency
                        && domain::normalize(a.s("name")) == domain::normalize(source.s("name"))
                })
                .collect::<Vec<_>>();
            if matches.len() == 1 {
                suggestion = Some(format!("asset:{}", matches[0].id("id")))
            }
        }
        if suggestion.is_none() {
            let name = domain::normalize(source.s("name"));
            let mask = source
                .s("mask")
                .chars()
                .rev()
                .take(4)
                .collect::<String>()
                .chars()
                .rev()
                .collect::<String>();
            let eligible = l
                .accounts
                .iter()
                .filter(|a| {
                    a.id("plaid_item_id") != 0
                        && a.s("iso_currency_code") == currency
                        && (source.s("class").is_empty()
                            || (["credit", "loan"].contains(&a.s("account_type"))
                                == (source.s("class") == "Liability")))
                        && (key.starts_with("legacy:")
                            || a.s("import_source_key").is_empty()
                            || a.s("import_source_key") == format!("tiller:{key}"))
                })
                .filter(|a| {
                    if key.starts_with("legacy:") {
                        ![
                            "checking",
                            "savings",
                            "credit card",
                            "mortgage loan",
                            "imported account",
                        ]
                        .contains(&name.as_str())
                            && [a.s("name"), a.s("official_name")]
                                .iter()
                                .any(|s| domain::normalize(s) == name)
                    } else {
                        mask.len() == 4
                            && mask.chars().all(|c| c.is_ascii_digit())
                            && a.s("mask") == mask
                            && !source.s("institution").is_empty()
                            && institutions.iter().any(|i| {
                                i.id("account_id") == a.id("id")
                                    && domain::normalize(i.s("institution"))
                                        == domain::normalize(source.s("institution"))
                            })
                    }
                })
                .collect::<Vec<_>>();
            if eligible.len() == 1 {
                suggestion = Some(eligible[0].id("id").to_string())
            }
        }
        let requested = settings["accounts"].s(&key);
        let selection = if requested == "new" && known.is_some() {
            known.clone().unwrap()
        } else if !requested.is_empty() {
            requested.to_string()
        } else if let Some(known) = &known {
            known.clone()
        } else if available_old {
            old.to_string()
        } else if let Some(s) = suggestion {
            s
        } else if !source.s("latest_at").is_empty() {
            "new".to_string()
        } else {
            "transactions_only".to_string()
        };
        if known.as_ref().is_some_and(|k| k != &selection) {
            errors.push(json!(format!(
                "{} was previously imported. Keep its existing account mapping.",
                source.s("name")
            )))
        }
        if !["new", "transactions_only"].contains(&selection.as_str()) {
            if let Some(asset) = l
                .assets
                .iter()
                .find(|a| format!("asset:{}", a.id("id")) == selection)
            {
                if asset.s("currency") != currency
                    || source.s("class") != "Asset"
                    || source.flag("hidden")
                {
                    errors.push(json!(format!(
                        "{} must be a visible asset in {currency} to map to {}.",
                        source.s("name"),
                        asset.s("name")
                    )))
                }
            } else if let Some(account) = l
                .accounts
                .iter()
                .find(|a| a.id("id").to_string() == selection)
            {
                if account.s("iso_currency_code") != currency
                    || (!source.s("class").is_empty()
                        && (["credit", "loan"].contains(&account.s("account_type"))
                            != (source.s("class") == "Liability")))
                {
                    errors.push(json!(format!(
                        "{} has a different currency or asset/liability classification.",
                        account.s("name")
                    )))
                }
                if !account.s("import_source_key").is_empty()
                    && account.s("import_source_key") != format!("tiller:{key}")
                    && !key.starts_with("legacy:")
                {
                    errors.push(json!(format!(
                        "{} is mapped to another Tiller account.",
                        account.s("name")
                    )))
                }
            } else {
                errors.push(json!(format!(
                    "Choose an available destination for {}.",
                    source.s("name")
                )))
            }
        }
        selections[&key] = json!(selection);
        source["selection"] = selections[&key].clone();
        source["mapping_note"] = json!(if known.is_some() || available_old {
            "Saved account choice"
        } else {
            "Review suggested destination"
        });
    }
    let mut daily = BTreeMap::<(String, String), Value>::new();
    for b in array(&data["balances"]) {
        let key = (b.s("account_key").to_string(), b.s("date").to_string());
        if daily.get(&key).is_none_or(|old| {
            (b.dec("timestamp"), b.id("row")) > (old.dec("timestamp"), old.id("row"))
        }) {
            daily.insert(key, b);
        }
    }
    let mut targets = HashMap::<String, HashSet<String>>::new();
    let mut balance_rows = vec![];
    for b in daily.values() {
        let selection = selections.s(b.s("account_key"));
        if !["new", "transactions_only"].contains(&selection) {
            targets
                .entry(selection.to_string())
                .or_default()
                .insert(b.s("account_key").to_string());
        }
        let status = if let Some(id) = selection
            .strip_prefix("asset:")
            .and_then(|s| s.parse::<i64>().ok())
        {
            let amount = b.dec("balance");
            if amount < Decimal::ZERO || amount.round_dp(2) != amount {
                errors.push(json!(format!("Asset valuation on {} requires a nonnegative amount with at most two decimal places.",b.s("date"))))
            }
            if l.valuations
                .iter()
                .any(|v| v.id("manual_asset_id") == id && v.s("valued_on") == b.s("date"))
            {
                "existing_asset_valuations_preserved"
            } else {
                "new_asset_valuations"
            }
        } else if selection == "transactions_only" {
            "skipped_balances"
        } else if l.snapshots.iter().any(|v| {
            v.id("account_id").to_string() == selection && v.s("as_of_date") == b.s("date")
        }) {
            "existing_daily_balances_preserved"
        } else {
            "new_balances"
        };
        let mut row = b.clone();
        row["status"] = json!(status);
        balance_rows.push(row);
    }
    if targets.values().any(|s| s.len() > 1) {
        errors.push(json!(
            "Two balance histories cannot share the same destination."
        ))
    }
    let mut rows = vec![];
    for mut row in array(&data["transactions"]) {
        let existing = l.transactions.iter().find(|t| {
            t.s("source") == "import"
                && t.s("source_transaction_id") == row.s("source_transaction_id")
        });
        if existing.is_some_and(|t| t.s("iso_currency_code") != currency) {
            errors.push(json!(
                "Workbook currency differs from previously imported transactions."
            ))
        };
        let provider_id = row["metadata"].s("Transaction ID");
        let conflict = existing.is_none()
            && !provider_id.is_empty()
            && l.transactions.iter().any(|t| {
                t.s("source") == "import"
                    && t["raw_payload"]["tiller"].s("Transaction ID") == provider_id
            });
        let mapped = selections
            .s(row.s("account_key"))
            .parse::<i64>()
            .unwrap_or(0);
        let mut reason = None;
        for t in &l.transactions {
            if t.id("parent_id") != 0
                || t.s("iso_currency_code") != currency
                || t.dec("amount") != row.dec("amount")
                || t.s("source_transaction_id") == row.s("source_transaction_id")
            {
                continue;
            }
            if mapped != 0
                && t.s("source") == "plaid"
                && t.id("account_id") == mapped
                && t.s("date") == row.s("date")
            {
                reason =
                    Some("Bank transaction has the same account, date and amount.".to_string());
                break;
            }
            let same = [t.s("name"), t.s("merchant_name")]
                .iter()
                .any(|name| domain::normalize(name) == domain::normalize(row.s("name")));
            if same
                && (t.s("date") == row.s("date")
                    || (t.s("source") == "plaid"
                        && (t.day("date") - row.day("date")).num_days().abs() <= 14))
            {
                reason = Some(format!(
                    "Existing transaction has the same amount and description ({}).",
                    t.s("date")
                ))
            }
        }
        if row.flag("file_duplicate") && reason.is_none() {
            reason = Some("Repeated source row in this workbook.".into())
        }
        if conflict {
            reason=Some("This Tiller ID has changed date, amount or account. Existing transaction kept; changed source row skipped.".into())
        }
        row["existing"] = json!(existing.is_some());
        row["conflict"] = json!(conflict);
        row["duplicate"] = json!(!conflict && reason.is_some());
        row["reason"] = json!(reason);
        rows.push(row);
    }
    let mut summary = json!({"transactions":rows.len(),"ready":rows.iter().filter(|r|!r.flag("existing")&&!r.flag("duplicate")&&!r.flag("conflict")).count(),"already_imported":rows.iter().filter(|r|r.flag("existing")).count(),"duplicates":rows.iter().filter(|r|r.flag("duplicate")&&!r.flag("existing")).count(),"changed_transactions":rows.iter().filter(|r|r.flag("conflict")).count(),"daily_balances":balance_rows.len(),"balance_observations":array(&data["balances"]).len(),"accounts":sources.len(),"categories":array(&data["categories"]).len()});
    for b in &balance_rows {
        let key = b.s("status");
        let count = summary[key].as_u64().unwrap_or(0);
        summary[key] = json!(count + 1)
    }
    Ok(
        json!({"rows":rows,"accounts":sources,"balances":balance_rows,"selections":selections,"errors":errors,"warnings":data["warnings"],"rules":data["rules"],"categories":data["categories"],"summary":summary,"category_mapping":category_mapping,"first_import":previous.is_empty()}),
    )
}
async fn commit_csv(
    conn: &mut PgConnection,
    batch: &Value,
    l: &Ledger,
    p: &Value,
) -> Result<Value> {
    let preview = csv_preview(batch, l)?;
    if !array(&preview["errors"]).is_empty() {
        bail!("Fix invalid rows and header mappings before importing")
    };
    let selected = array(&p["include_duplicates"])
        .iter()
        .map(|v| {
            v.as_str()
                .map(str::to_string)
                .unwrap_or_else(|| v.to_string())
        })
        .collect::<HashSet<_>>();
    let mut count = 0;
    let mut duplicate_count = 0;
    for row in array(&preview["rows"]) {
        if row.flag("existing")
            || row.flag("duplicate") && !selected.contains(&row.id("number").to_string())
        {
            continue;
        }
        let category = batch["settings"]["categories"][row.s("source_category")]
            .as_str()
            .and_then(|s| s.parse::<i64>().ok());
        if category.is_some_and(|id| l.category(id).is_none()) {
            bail!("Category mapping is unavailable")
        };
        let mut attrs = crate::actions::select(
            &row,
            &[
                "date",
                "name",
                "amount",
                "source_account_name",
                "notes",
                "iso_currency_code",
                "source_transaction_id",
            ],
        );
        attrs["source"] = json!("import");
        attrs["tags"] = json!(row.s("tags"));
        attrs["user_id"] = json!(l.user);
        attrs["duplicate"] = json!(row.flag("duplicate"));
        attrs["raw_payload"] = json!({"import_category":row["source_category"]});
        if let Some(category) = category {
            attrs["category_id"] = json!(category);
            attrs["category_locked"] = json!(true);
            attrs["category_origin"] = json!("import");
            attrs["category_reason"] = json!("Your CSV category mapping")
        };
        for rule in &l.rules {
            domain::apply_rule(rule, &mut attrs)
        }
        db::save(conn, "transactions", l.user, None, attrs).await?;
        count += 1;
        if row.flag("duplicate") {
            duplicate_count += 1
        }
    }
    if !p.s("profile_name").trim().is_empty() {
        let existing = db::rows(conn, "import_profiles", l.user)
            .await?
            .iter()
            .find(|v| v.s("name") == p.s("profile_name"))
            .map(|v| v.id("id"));
        let mut settings = batch["settings"].clone();
        settings["headers"] = preview["mapping"].clone();
        db::save(
            conn,
            "import_profiles",
            l.user,
            existing,
            json!({"name":p.s("profile_name"),"settings":settings}),
        )
        .await?;
    }
    domain::refresh(conn, l.user).await?;
    Ok(json!({"transactions":count,"duplicates":duplicate_count,"balances":0}))
}
fn increment(result: &mut Value, k: &str) {
    result[k] = json!(result[k].as_u64().unwrap_or(0) + 1)
}
async fn commit_tiller(conn: &mut PgConnection, batch: &Value, l: &Ledger) -> Result<Value> {
    let preview = tiller_preview(conn, batch, l).await?;
    if let Some(error) = array(&preview["errors"]).first() {
        bail!("{}", error.as_str().unwrap_or("Fix import errors"))
    };
    let rows = array(&preview["rows"]);
    let balances = array(&preview["balances"]);
    if rows.is_empty() && balances.is_empty() {
        bail!("Workbook has no transactions or balances")
    };
    let settings = &batch["settings"];
    let currency = settings.s("currency");
    let mut result = json!({"transactions":0,"balances":0,"accounts":0,"categories":0,"duplicates":0,"skipped":0,"changed_transactions":0,"already_imported":0,"skipped_duplicates":0,"asset_valuations":0,"existing_daily_balances_preserved":0,"existing_asset_valuations_preserved":0,"skipped_balances":0});
    let mut selections = preview["selections"].clone();
    let mut account_map = HashMap::<String, Option<i64>>::new();
    for source in array(&preview["accounts"]) {
        let selection = selections.s(source.s("key")).to_string();
        let id = if selection == "new" {
            let typ = if source.s("class") == "Liability" {
                if source.s("type").to_uppercase() == "CREDIT" {
                    "credit"
                } else {
                    "loan"
                }
            } else {
                match source.s("type").to_uppercase().as_str() {
                    "INDIVIDUAL" | "INVESTMENT" | "BROKERAGE_CASH" => "investment",
                    "CHECKING" | "SAVINGS" | "MONEY_MARKET" | "DIGITAL_WALLET" => "depository",
                    _ => "other",
                }
            };
            let a=db::save(conn,"accounts",l.user,None,json!({"import_source_key":format!("tiller:{}",source.s("key")),"name":source["name"],"mask":source.s("mask").chars().rev().take(4).collect::<String>().chars().rev().collect::<String>(),"account_type":typ,"iso_currency_code":currency,"net_worth_excluded":source.flag("hidden"),"import_metadata":source})).await?;
            increment(&mut result, "accounts");
            Some(a.id("id"))
        } else if selection == "transactions_only" || selection.starts_with("asset:") {
            None
        } else {
            let id = selection.parse::<i64>()?;
            let a = l
                .account(id)
                .ok_or_else(|| anyhow::anyhow!("Account is unavailable"))?;
            if a.s("import_source_key").is_empty() && !source.s("key").starts_with("legacy:") {
                db::save(conn,"accounts",l.user,Some(id),json!({"import_source_key":format!("tiller:{}",source.s("key")),"import_metadata":source})).await?;
            }
            Some(id)
        };
        if let Some(id) = id {
            selections[source.s("key")] = json!(id.to_string())
        }
        account_map.insert(source.s("key").to_string(), id);
    }
    let importable = |r: &Value| {
        !r.flag("existing")
            && !r.flag("conflict")
            && (!r.flag("duplicate") || settings.s("include_duplicates") == "1")
    };
    let needed = rows
        .iter()
        .filter(|r| importable(r))
        .map(|r| r.s("category"))
        .collect::<HashSet<_>>();
    let mut mapping = preview["category_mapping"].clone();
    let mut categories = l.categories.clone();
    for source in array(&preview["categories"]) {
        if !preview.flag("first_import") && !needed.contains(source.s("name")) {
            continue;
        }
        let existing_id = mapping.s(source.s("name")).parse::<i64>().unwrap_or(0);
        if categories
            .iter()
            .any(|c| c.id("id") == existing_id && c.id("user_id") == l.user)
        {
            continue;
        }
        let parent = if !source.s("group").is_empty() && source.s("group") != source.s("name") {
            let found = categories
                .iter()
                .find(|c| {
                    c.id("user_id") == l.user
                        && c.s("name") == source.s("group")
                        && c.id("parent_id") == 0
                })
                .map(|c| c.id("id"));
            Some(if let Some(id) = found {
                id
            } else {
                let c = db::save(
                    conn,
                    "categories",
                    l.user,
                    None,
                    json!({"name":source.s("group")}),
                )
                .await?;
                let id = c.id("id");
                categories.push(c);
                increment(&mut result, "categories");
                id
            })
        } else {
            None
        };
        let found = categories
            .iter()
            .find(|c| {
                c.id("user_id") == l.user
                    && c.s("name") == source.s("name")
                    && c.id("parent_id") == parent.unwrap_or(0)
            })
            .map(|c| c.id("id"));
        let id = if let Some(id) = found {
            id
        } else {
            let c = db::save(
                conn,
                "categories",
                l.user,
                None,
                json!({"name":source.s("name"),"parent_id":parent}),
            )
            .await?;
            let id = c.id("id");
            categories.push(c);
            increment(&mut result, "categories");
            id
        };
        mapping[source.s("name")] = json!(id.to_string());
    }
    for row in &rows {
        if !importable(row) {
            increment(&mut result, "skipped");
            increment(
                &mut result,
                if row.flag("existing") {
                    "already_imported"
                } else if row.flag("conflict") {
                    "changed_transactions"
                } else {
                    "skipped_duplicates"
                },
            );
            continue;
        }
        let mut attrs = crate::actions::select(
            row,
            &[
                "date",
                "name",
                "amount",
                "source_account_name",
                "source_transaction_id",
                "original_description",
                "notes",
                "tags",
                "hidden",
                "transfer",
            ],
        );
        attrs["source"] = json!("import");
        attrs["tags"] = json!(row.s("tags"));
        attrs["account_id"] = json!(account_map[row.s("account_key")]);
        attrs["iso_currency_code"] = json!(currency);
        attrs["category_id"] = json!(mapping.s(row.s("category")).parse::<i64>().ok());
        attrs["category_locked"] = json!(true);
        attrs["category_origin"] = json!("import");
        attrs["category_reason"] = json!("Your Tiller category");
        attrs["duplicate"] = json!(row.flag("duplicate"));
        attrs["classification_override"] = json!(true);
        attrs["classification_origin"] = json!("import");
        attrs["classification_reason"] = json!("Your Tiller category type");
        attrs["raw_payload"] = json!({"import_category":row["category"],"tiller":row["metadata"],"import_duplicate_reason":row["reason"]});
        db::save(conn, "transactions", l.user, None, attrs).await?;
        increment(&mut result, "transactions");
        if row.flag("duplicate") {
            increment(&mut result, "duplicates")
        }
    }
    let mut changed_accounts = HashSet::new();
    let mut changed_assets = HashSet::new();
    for b in balances {
        let selection = selections.s(b.s("account_key"));
        if let Some(asset) = selection
            .strip_prefix("asset:")
            .and_then(|s| s.parse::<i64>().ok())
        {
            let count=sqlx::query("INSERT INTO asset_valuations(manual_asset_id,value,valued_on) VALUES($1,$2,$3) ON CONFLICT(manual_asset_id,valued_on) DO NOTHING").bind(asset).bind(b.dec("balance")).bind(b.day("date")).execute(&mut *conn).await?.rows_affected();
            increment(
                &mut result,
                if count > 0 {
                    "asset_valuations"
                } else {
                    "existing_asset_valuations_preserved"
                },
            );
            changed_assets.insert(asset);
        } else if let Some(account) = account_map[b.s("account_key")] {
            let exists = l
                .snapshots
                .iter()
                .any(|s| s.id("account_id") == account && s.s("as_of_date") == b.s("date"));
            if exists {
                increment(&mut result, "existing_daily_balances_preserved")
            } else {
                db::save(conn,"balance_snapshots",l.user,None,json!({"account_id":account,"as_of_date":b["date"],"current_balance":b["balance"],"iso_currency_code":currency,"import_metadata":b})).await?;
                increment(&mut result, "balances");
                changed_accounts.insert(account);
            }
        } else {
            increment(&mut result, "skipped_balances")
        }
    }
    for id in changed_accounts {
        let account = db::owned(conn, "accounts", l.user, id).await?;
        if account.id("plaid_item_id") != 0 {
            continue;
        }
        let latest:Value=sqlx::query_scalar("SELECT to_jsonb(s) FROM balance_snapshots s WHERE user_id=$1 AND account_id=$2 ORDER BY as_of_date DESC LIMIT 1").bind(l.user).bind(id).fetch_one(&mut *conn).await?;
        let mut metadata = account["import_metadata"].clone();
        metadata["as_of_date"] = latest["as_of_date"].clone();
        db::save(
            conn,
            "accounts",
            l.user,
            Some(id),
            json!({"current_balance":latest["current_balance"],"import_metadata":metadata}),
        )
        .await?;
    }
    for id in changed_assets {
        let asset = db::owned(conn, "manual_assets", l.user, id).await?;
        let latest:Value=sqlx::query_scalar("SELECT to_jsonb(v) FROM asset_valuations v WHERE manual_asset_id=$1 ORDER BY valued_on DESC LIMIT 1").bind(id).fetch_one(&mut *conn).await?;
        if latest.s("valued_on") > asset.s("valued_on") {
            db::save(
                conn,
                "manual_assets",
                l.user,
                Some(id),
                json!({"value":latest["value"],"valued_on":latest["valued_on"]}),
            )
            .await?;
        }
    }
    Ok(
        json!({"result":result,"accounts":selections,"category_mapping":mapping,"summary":preview["summary"],"warnings":preview["warnings"],"autocat_reference":preview["rules"],"changed_source_rows":rows.iter().filter(|r|r.flag("conflict")).map(|r|crate::actions::select(r,&["row","name","date","reason"])).collect::<Vec<_>>()}),
    )
}
pub async fn handle(state: &AppState, user: i64, path: &str, input: Input) -> Result<Response> {
    let p = &input.params;
    let mut tx = state.pool.begin().await?;
    sqlx::query("SELECT id FROM users WHERE id=$1 FOR UPDATE")
        .bind(user)
        .execute(&mut *tx)
        .await?;
    if path == "/imports" {
        let (name, bytes) = input
            .uploads
            .get("file")
            .ok_or_else(|| anyhow::anyhow!("Choose a CSV or XLSX file"))?;
        let (text, settings) = if name.to_lowercase().ends_with(".xlsx") {
            let bytes = bytes.clone();
            let data = tokio::task::spawn_blocking(move || read_tiller(bytes)).await??;
            (
                data.to_string(),
                json!({"format":"tiller","currency":"USD","include_duplicates":"1","source_filename":name.rsplit(['/', '\\']).next().unwrap_or(name)}),
            )
        } else {
            if bytes.len() > 5 * 1024 * 1024 {
                bail!("Use a UTF-8 CSV under 5 MB")
            };
            let text = String::from_utf8(bytes.clone())?;
            let settings = if p.id("profile_id") != 0 {
                db::owned(&mut tx, "import_profiles", user, p.id("profile_id")).await?["settings"]
                    .clone()
            } else {
                json!({"currency":"USD"})
            };
            (text, settings)
        };
        let batch = db::save(
            &mut tx,
            "import_batches",
            user,
            None,
            json!({"csv_text":text,"settings":settings}),
        )
        .await?;
        tx.commit().await?;
        return Ok(web::redirect(
            &format!("/imports/{}", batch.id("id")),
            "Review your import before saving.",
        ));
    }
    let id = path.rsplit('/').next().unwrap_or("").parse::<i64>()?;
    let mut batch = db::owned(&mut tx, "import_batches", user, id).await?;
    if !batch["imported_at"].is_null() {
        bail!("This batch was already imported")
    };
    let tiller = batch["settings"].s("format") == "tiller";
    for key in if tiller {
        vec!["currency", "include_duplicates", "accounts"]
    } else {
        vec!["currency", "reverse_sign", "headers", "categories"]
    } {
        if !p[key].is_null() {
            batch["settings"][key] = p[key].clone()
        }
    }
    if !domain::CURRENCIES.contains(&batch["settings"].s("currency")) {
        bail!("Choose a supported currency")
    };
    if p.get("commit_import").is_some() {
        let l = Ledger::load(&mut tx, user).await?;
        if tiller {
            let receipt = commit_tiller(&mut tx, &batch, &l).await?;
            for (k, v) in receipt.as_object().unwrap() {
                batch["settings"][k] = v.clone()
            }
        } else {
            batch["settings"]["result"] = commit_csv(&mut tx, &batch, &l, p).await?;
        }
        batch["csv_text"] = json!("Imported (source file cleared)");
        batch["imported_at"] = json!(chrono::Utc::now().naive_utc());
    }
    db::save(&mut tx, "import_batches", user, Some(id), batch).await?;
    tx.commit().await?;
    Ok(web::redirect(
        &format!("/imports/{id}"),
        if p.get("commit_import").is_some() {
            "Import completed."
        } else {
            "Import preview updated."
        },
    ))
}
