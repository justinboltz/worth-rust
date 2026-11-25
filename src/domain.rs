use crate::db;
use anyhow::{Result, bail};
use chrono::{Datelike, Duration, Months, NaiveDate, Utc};
use rust_decimal::Decimal;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::PgConnection;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::str::FromStr;

pub const CURRENCIES: [&str; 4] = ["USD", "CAD", "EUR", "GBP"];
pub const FLAGS: [&str; 6] = [
    "hidden",
    "duplicate",
    "transfer",
    "card_payment",
    "pending",
    "split_needs_review",
];
pub trait Record {
    fn s(&self, key: &str) -> &str;
    fn id(&self, key: &str) -> i64;
    fn flag(&self, key: &str) -> bool;
    fn dec(&self, key: &str) -> Decimal;
    fn day(&self, key: &str) -> NaiveDate;
}
impl Record for Value {
    fn s(&self, k: &str) -> &str {
        self[k].as_str().unwrap_or("")
    }
    fn id(&self, k: &str) -> i64 {
        self[k]
            .as_i64()
            .or_else(|| self.s(k).parse().ok())
            .unwrap_or(0)
    }
    fn flag(&self, k: &str) -> bool {
        self[k]
            .as_bool()
            .unwrap_or_else(|| matches!(self.s(k), "1" | "true" | "on"))
    }
    fn dec(&self, k: &str) -> Decimal {
        decimal(&self[k]).unwrap_or(Decimal::ZERO)
    }
    fn day(&self, k: &str) -> NaiveDate {
        date(self.s(k)).unwrap_or(NaiveDate::MIN)
    }
}
pub fn decimal(v: &Value) -> Result<Decimal> {
    Ok(Decimal::from_str(&if let Some(s) = v.as_str() {
        s.to_owned()
    } else {
        v.to_string()
    })?)
}
pub fn amount(s: &str) -> Result<Decimal> {
    let n = Decimal::from_str(s.trim())?;
    if n.scale() > 4 || n.abs() >= Decimal::from(1_000_000_000_000_000i64) {
        bail!("Amount must have at most four decimal places and fit the ledger")
    }
    Ok(n)
}
pub fn date(s: &str) -> Result<NaiveDate> {
    Ok(NaiveDate::parse_from_str(s, "%Y-%m-%d")?)
}
pub fn today() -> NaiveDate {
    Utc::now().date_naive()
}
pub fn normalize(s: &str) -> String {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}
pub fn hash(s: &str) -> String {
    format!("{:x}", Sha256::digest(s.as_bytes()))
}
pub fn month_start(d: NaiveDate) -> NaiveDate {
    d.with_day(1).unwrap()
}
pub fn month_end(d: NaiveDate) -> NaiveDate {
    month_start(d).checked_add_months(Months::new(1)).unwrap() - Duration::days(1)
}
pub fn merchant_key(t: &Value) -> Option<String> {
    let prefix = if t.dec("amount") < Decimal::ZERO {
        "inflow"
    } else {
        "outflow"
    };
    let identity = if !t.s("merchant_entity_id").is_empty() {
        format!("entity:{}", t.s("merchant_entity_id"))
    } else if !t.s("merchant_name").is_empty() {
        format!("merchant:{}", normalize(t.s("merchant_name")))
    } else {
        let s = normalize(t.s("name"));
        if s.len() < 4
            || [
                "purchase",
                "debit card purchase",
                "card purchase",
                "payment",
                "online payment",
                "deposit",
                "withdrawal",
                "transfer",
                "transaction",
                "unknown",
                "check",
                "pos",
            ]
            .contains(&s.as_str())
        {
            return None;
        }
        format!("description:{s}")
    };
    Some(format!("{prefix}:{identity}"))
}
pub fn excluded(t: &Value) -> bool {
    FLAGS.iter().any(|k| t.flag(k))
}
pub fn rule_matches(rule: &Value, t: &Value) -> bool {
    if rule.id("user_id") != t.id("user_id") {
        return false;
    }
    let mut conditions = vec![];
    if !rule.s("merchant_key").is_empty() {
        conditions.push(merchant_key(t).as_deref() == Some(rule.s("merchant_key")))
    } else if !rule.s("match_text").is_empty() {
        let a = t.s("name").to_lowercase();
        let b = rule.s("match_text").to_lowercase();
        conditions.push(match rule.s("description_operator") {
            "equals" => a == b,
            "starts_with" => a.starts_with(&b),
            "ends_with" => a.ends_with(&b),
            "contains" => a.contains(&b),
            _ => false,
        })
    }
    if rule.id("account_id") != 0 {
        conditions.push(rule.id("account_id") == t.id("account_id"))
    }
    if !rule.s("source_account_name").is_empty() {
        conditions
            .push(normalize(rule.s("source_account_name")) == normalize(t.s("source_account_name")))
    }
    !conditions.is_empty() && conditions.iter().all(|v| *v)
}
pub fn apply_rule(rule: &Value, t: &mut Value) {
    if !rule_matches(rule, t) {
        return;
    }
    if rule.id("category_id") != 0
        && !(t.flag("category_locked")
            && ["manual", "import", "existing", "split"].contains(&t.s("category_origin")))
    {
        t["category_id"] = rule["category_id"].clone();
        t["category_origin"] = json!("rule");
        t["category_locked"] = json!(true);
        t["category_reason"] = json!("Your saved rule");
        t["suggested_category_id"] = Value::Null;
        t["category_suggestion_reason"] = Value::Null;
    }
    t["tags"] = json!(tags(&format!("{},{}", t.s("tags"), rule.s("tags"))));
    if ["transfer", "card_payment"].contains(&rule.s("kind"))
        && t.s("classification_origin") != "manual"
    {
        t["classification_override"] = json!(true);
        t["classification_origin"] = json!("rule");
        t["classification_reason"] = json!("Your saved rule");
        t["matched_transaction_id"] = Value::Null;
        t["suggested_classification"] = Value::Null;
        t["suggestion_reason"] = Value::Null;
        t[rule.s("kind")] = json!(true);
    }
    if rule.s("kind") == "hidden" {
        t["hidden"] = json!(true)
    }
}
pub fn tags(s: &str) -> String {
    let mut seen = HashSet::new();
    s.split(',')
        .map(str::trim)
        .filter(|v| !v.is_empty() && seen.insert(*v))
        .collect::<Vec<_>>()
        .join(", ")
}
#[derive(Default, Clone)]
pub struct Ledger {
    pub user: i64,
    pub transactions: Vec<Value>,
    pub accounts: Vec<Value>,
    pub categories: Vec<Value>,
    pub rules: Vec<Value>,
    pub budgets: Vec<Value>,
    pub recurring: Vec<Value>,
    pub dismissals: Vec<Value>,
    pub assets: Vec<Value>,
    pub expenses: Vec<Value>,
    pub snapshots: Vec<Value>,
    pub valuations: Vec<Value>,
}
impl Ledger {
    pub async fn load(conn: &mut PgConnection, user: i64) -> Result<Self> {
        Ok(Self{user,transactions:db::rows(conn,"transactions",user).await?,accounts:db::rows(conn,"accounts",user).await?,categories:db::rows(conn,"categories",user).await?,rules:db::rows(conn,"transaction_rules",user).await?,budgets:db::rows(conn,"budgets",user).await?,recurring:db::rows(conn,"recurring_items",user).await?,dismissals:db::rows(conn,"recurring_dismissals",user).await?,assets:db::rows(conn,"manual_assets",user).await?,expenses:db::rows(conn,"category_expense_types",user).await?,snapshots:db::rows(conn,"balance_snapshots",user).await?,valuations:sqlx::query_scalar("SELECT to_jsonb(v) FROM asset_valuations v JOIN manual_assets a ON a.id=v.manual_asset_id WHERE a.user_id=$1 ORDER BY v.valued_on").bind(user).fetch_all(conn).await?})
    }
    pub fn account(&self, id: i64) -> Option<&Value> {
        self.accounts.iter().find(|v| v.id("id") == id)
    }
    pub fn category(&self, id: i64) -> Option<&Value> {
        self.categories.iter().find(|v| v.id("id") == id)
    }
    pub fn transaction(&self, id: i64) -> Option<&Value> {
        self.transactions.iter().find(|v| v.id("id") == id)
    }
    pub fn tracked(&self, t: &Value) -> bool {
        t.id("account_id") == 0
            || self
                .account(t.id("account_id"))
                .is_some_and(|a| a.flag("tracked"))
    }
    pub fn reportable(&self, t: &Value) -> bool {
        self.tracked(t)
            && !excluded(t)
            && !t.flag("is_split")
            && (t.id("parent_id") == 0
                || self
                    .transaction(t.id("parent_id"))
                    .is_some_and(|p| self.tracked(p) && !excluded(p)))
    }
    pub fn category_chain(&self, id: i64) -> Vec<&Value> {
        let mut out = vec![];
        let mut next = id;
        let mut seen = HashSet::new();
        while next != 0 && seen.insert(next) {
            if let Some(c) = self.category(next) {
                out.push(c);
                next = c.id("parent_id")
            } else {
                break;
            }
        }
        out
    }
    pub fn category_name(&self, id: i64) -> String {
        let mut chain = self.category_chain(id);
        chain.reverse();
        chain
            .iter()
            .map(|c| c.s("name"))
            .collect::<Vec<_>>()
            .join(" > ")
    }
    pub fn subtree(&self, id: i64) -> HashSet<i64> {
        self.categories
            .iter()
            .filter(|c| {
                self.category_chain(c.id("id"))
                    .iter()
                    .any(|p| p.id("id") == id)
            })
            .map(|c| c.id("id"))
            .collect()
    }
    pub fn category_expense(&self, id: i64) -> &str {
        let chain = self.category_chain(id);
        for c in &chain {
            if let Some(e) = self
                .expenses
                .iter()
                .find(|v| v.id("category_id") == c.id("id"))
            {
                return e.s("expense_type");
            }
        }
        for c in chain {
            if !c.flag("system") {
                continue;
            }
            if ["Rent", "Internet", "Mobile Phone"].contains(&c.s("name")) {
                return "fixed";
            }
            if [
                "Electricity",
                "Gas",
                "Water",
                "Food & Dining",
                "Shopping",
                "Gas & Fuel",
                "Parking",
                "Public Transportation",
                "Entertainment",
                "Travel",
            ]
            .contains(&c.s("name"))
            {
                return "variable";
            }
        }
        "unclassified"
    }
    pub fn expense<'a>(&'a self, t: &'a Value) -> &'a str {
        if !t.s("expense_type").is_empty() {
            return t.s("expense_type");
        }
        if let Some(p) = self.transaction(t.id("parent_id")) {
            if !p.s("expense_type").is_empty() {
                return p.s("expense_type");
            }
        }
        self.category_expense(t.id("category_id"))
    }
    pub fn decorated(&self, t: &Value) -> Value {
        let mut v = t.clone();
        v["category_name"] = json!(self.category_name(t.id("category_id")));
        v["account_name"] = json!(if !t.s("source_account_name").is_empty() {
            t.s("source_account_name")
        } else {
            self.account(t.id("account_id"))
                .map_or("Manual / cash", |a| a.s("name"))
        });
        v["expense_label"] = json!(self.expense(t));
        v["reportable"] = json!(self.reportable(t));
        v["root_id"] = json!(if t.id("parent_id") != 0 {
            t.id("parent_id")
        } else {
            t.id("id")
        });
        for flag in FLAGS {
            if self
                .transaction(t.id("parent_id"))
                .is_some_and(|p| p.flag(flag))
            {
                v[flag] = json!(true)
            }
        }
        v
    }
    pub fn search(&self, p: &Value) -> Result<Vec<Value>> {
        for key in ["from", "to"] {
            if !p.s(key).is_empty() {
                date(p.s(key))?;
            }
        }
        if !p.s("from").is_empty() && !p.s("to").is_empty() && p.s("from") > p.s("to") {
            bail!("End date must be on or after start date")
        }
        let min = if p.s("min_amount").is_empty() {
            None
        } else {
            Some(amount(p.s("min_amount"))?)
        };
        let max = if p.s("max_amount").is_empty() {
            None
        } else {
            Some(amount(p.s("max_amount"))?)
        };
        if min.is_some_and(|v| v < Decimal::ZERO)
            || max.is_some_and(|v| v < Decimal::ZERO)
            || min.zip(max).is_some_and(|(a, b)| a > b)
        {
            bail!("Choose a valid nonnegative amount range")
        }
        if !p.s("currency").is_empty() && !CURRENCIES.contains(&p.s("currency")) {
            bail!("Choose a supported currency")
        }
        if !p.s("source").is_empty() && !["plaid", "import", "manual"].contains(&p.s("source")) {
            bail!("Choose a valid source")
        }
        if !p.s("expense_type").is_empty()
            && !["fixed", "variable", "unclassified"].contains(&p.s("expense_type"))
        {
            bail!("Choose a valid expense type")
        }
        if !p.s("account_id").is_empty()
            && !self
                .account(p.id("account_id"))
                .is_some_and(|a| a.flag("tracked"))
        {
            bail!("Choose one of your tracked accounts")
        }
        if !p.s("category_id").is_empty()
            && p.s("category_id") != "uncategorized"
            && self.category(p.id("category_id")).is_none()
        {
            bail!("Choose an available category")
        }
        let subtree = self.subtree(p.id("category_id"));
        let q = normalize(p.s("q"));
        let mut rows: Vec<Value> = self
            .transactions
            .iter()
            .filter(|t| self.tracked(t) && !t.flag("is_split"))
            .map(|t| self.decorated(t))
            .filter(|t| {
                (q.is_empty()
                    || [
                        "name",
                        "merchant_name",
                        "original_description",
                        "notes",
                        "tags",
                        "source_account_name",
                        "account_name",
                        "category_name",
                    ]
                    .iter()
                    .any(|k| normalize(t.s(k)).contains(&q)))
                    && (p.s("account_id").is_empty() || t.id("account_id") == p.id("account_id"))
                    && (p.s("source_account_name").is_empty()
                        || t.s("source_account_name") == p.s("source_account_name"))
                    && (p.s("category_id").is_empty()
                        || if p.s("category_id") == "uncategorized" {
                            t.id("category_id") == 0
                        } else {
                            subtree.contains(&t.id("category_id"))
                        })
                    && (p.s("from").is_empty() || t.s("date") >= p.s("from"))
                    && (p.s("to").is_empty() || t.s("date") <= p.s("to"))
                    && (p.s("currency").is_empty() || t.s("iso_currency_code") == p.s("currency"))
                    && (p.s("source").is_empty() || t.s("source") == p.s("source"))
                    && (p.s("tag").is_empty()
                        || t.s("tags")
                            .split(',')
                            .any(|s| normalize(s) == normalize(p.s("tag"))))
                    && (p.s("flow") != "inflow" || t.dec("amount") < Decimal::ZERO)
                    && (p.s("flow") != "outflow" || t.dec("amount") > Decimal::ZERO)
                    && min.is_none_or(|v| t.dec("amount").abs() >= v)
                    && max.is_none_or(|v| t.dec("amount").abs() <= v)
                    && (p.s("reportable") != "1" || t.flag("reportable"))
                    && (p.s("expense_type").is_empty()
                        || (t.dec("amount") > Decimal::ZERO
                            && t.s("expense_label") == p.s("expense_type")))
                    && (match p.s("flag") {
                        "category_review" => !t.s("category_suggestion_reason").is_empty(),
                        "suggested" => !t.s("suggested_classification").is_empty(),
                        k if FLAGS.contains(&k) => t.flag(k),
                        _ => true,
                    })
            })
            .collect();
        rows.sort_by(|a, b| {
            let cmp = match p.s("sort") {
                "amount" => a.dec("amount").cmp(&b.dec("amount")),
                "name" => a.s("name").cmp(b.s("name")),
                _ => a.s("date").cmp(b.s("date")),
            }
            .then(a.id("id").cmp(&b.id("id")));
            if p.s("direction") == "asc" {
                cmp
            } else {
                cmp.reverse()
            }
        });
        Ok(rows)
    }
    pub fn report(&self, from: NaiveDate, to: NaiveDate, currency: &str) -> Value {
        let mut spending = Decimal::ZERO;
        let mut income = Decimal::ZERO;
        let mut categories = BTreeMap::<String, Decimal>::new();
        let mut monthly = BTreeMap::<String, (Decimal, Decimal)>::new();
        let mut types = BTreeMap::from([
            ("fixed", Decimal::ZERO),
            ("variable", Decimal::ZERO),
            ("unclassified", Decimal::ZERO),
        ]);
        for t in &self.transactions {
            if !self.reportable(t)
                || t.day("date") < from
                || t.day("date") > to
                || t.s("iso_currency_code") != currency
            {
                continue;
            }
            let a = t.dec("amount");
            let m = monthly.entry(t.s("date")[..7].to_string()).or_default();
            if a > Decimal::ZERO {
                spending += a;
                m.0 += a;
                let name = self
                    .category_chain(t.id("category_id"))
                    .last()
                    .map_or("Uncategorized", |c| c.s("name"))
                    .to_string();
                *categories.entry(name).or_default() += a;
                *types.entry(self.expense(t)).or_default() += a;
            } else {
                income -= a;
                m.1 -= a
            }
        }
        let mut groups = categories
            .into_iter()
            .map(|(name, total)| json!({"name":name,"amount":total.to_string()}))
            .collect::<Vec<_>>();
        groups.sort_by_key(|b| std::cmp::Reverse(b.dec("amount")));
        json!({"spending":spending.to_string(),"income":income.to_string(),"net":(income-spending).to_string(),"categories":groups,"expense_types":types.into_iter().map(|(name,a)|json!({"name":name,"amount":a.to_string()})).collect::<Vec<_>>(),"monthly":monthly.into_iter().map(|(month,(s,i))|json!({"month":month,"spending":s.to_string(),"income":i.to_string(),"net":(i-s).to_string()})).collect::<Vec<_>>()})
    }
    pub fn budget_summary(&self, b: &Value, month: NaiveDate) -> Value {
        let ids = self.subtree(b.id("category_id"));
        let mut prior = Decimal::ZERO;
        let mut spent = Decimal::ZERO;
        for t in &self.transactions {
            if self.reportable(t)
                && ids.contains(&t.id("category_id"))
                && t.s("iso_currency_code") == b.s("currency")
                && t.dec("amount") > Decimal::ZERO
            {
                if t.day("date") >= month && t.day("date") <= month_end(month) {
                    spent += t.dec("amount")
                } else if t.day("date") >= b.day("starts_on") && t.day("date") < month {
                    prior += t.dec("amount")
                }
            }
        }
        let months = (month.year() - b.day("starts_on").year()) * 12 + month.month() as i32
            - b.day("starts_on").month() as i32;
        let available = if months < 0 {
            Decimal::ZERO
        } else if b.flag("rollover") {
            b.dec("amount") * Decimal::from(months + 1) - prior
        } else {
            b.dec("amount")
        };
        let mut v = b.clone();
        v["category_name"] = json!(self.category_name(b.id("category_id")));
        v["spent"] = json!(spent.to_string());
        v["available"] = json!(available.to_string());
        v["remaining"] = json!((available - spent).to_string());
        v
    }
    pub fn net_worth(&self, currency: &str, from: NaiveDate, to: NaiveDate) -> Vec<Value> {
        let accounts = self
            .accounts
            .iter()
            .filter(|a| {
                a.flag("tracked")
                    && !a.flag("net_worth_excluded")
                    && a.s("iso_currency_code") == currency
            })
            .map(|a| (a.id("id"), a))
            .collect::<HashMap<_, _>>();
        let assets = self
            .assets
            .iter()
            .filter(|a| a.s("currency") == currency)
            .map(|a| a.id("id"))
            .collect::<HashSet<_>>();
        let mut snapshots = self
            .snapshots
            .iter()
            .filter(|s| {
                accounts.contains_key(&s.id("account_id"))
                    && s.s("iso_currency_code") == currency
                    && s.day("as_of_date") <= to
            })
            .collect::<Vec<_>>();
        let mut valuations = self
            .valuations
            .iter()
            .filter(|v| assets.contains(&v.id("manual_asset_id")) && v.day("valued_on") <= to)
            .collect::<Vec<_>>();
        snapshots.sort_by_key(|s| s.day("as_of_date"));
        valuations.sort_by_key(|v| v.day("valued_on"));
        let mut dates = snapshots
            .iter()
            .map(|s| s.day("as_of_date"))
            .chain(valuations.iter().map(|v| v.day("valued_on")))
            .filter(|d| *d >= from)
            .collect::<Vec<_>>();
        dates.sort();
        dates.dedup();
        let (mut i, mut j) = (0, 0);
        let mut known = HashMap::new();
        let mut known_assets = HashMap::new();
        let mut output = vec![];
        for day in dates {
            while i < snapshots.len() && snapshots[i].day("as_of_date") <= day {
                let s = snapshots[i];
                if !s["current_balance"].is_null() {
                    known.insert(
                        s.id("account_id"),
                        signed_balance(accounts[&s.id("account_id")], s.dec("current_balance")),
                    );
                }
                i += 1;
            }
            while j < valuations.len() && valuations[j].day("valued_on") <= day {
                let v = valuations[j];
                known_assets.insert(v.id("manual_asset_id"), v.dec("value"));
                j += 1;
            }
            let value: Decimal = known.values().chain(known_assets.values()).copied().sum();
            output.push(json!({"date":day,"value":value.to_string(),"known_accounts":known.len(),"total_accounts":accounts.len(),"known_assets":known_assets.len(),"total_assets":assets.len()}));
        }
        output
    }
    pub fn recurring_suggestions(&self, now: NaiveDate) -> Vec<Value> {
        let mut histories = HashMap::<String, Vec<&Value>>::new();
        for t in &self.transactions {
            if t.id("parent_id") == 0
                && self.tracked(t)
                && !excluded(t)
                && t.dec("amount") != Decimal::ZERO
                && t.day("date") <= now
                && t.day("date") >= now - Duration::days(1096)
                && CURRENCIES.contains(&t.s("iso_currency_code"))
            {
                if let Some(key) = detection_key(t) {
                    histories.entry(key).or_default().push(t)
                }
            }
        }
        let mut suggestions = vec![];
        for (key, mut rows) in histories {
            if self
                .recurring
                .iter()
                .chain(&self.dismissals)
                .any(|r| r.s("detection_key") == key)
            {
                continue;
            }
            rows.sort_by_key(|r| (r.day("date"), r.id("id")));
            let recent = &rows[rows.len().saturating_sub(6)..];
            if recent.len() < 3 {
                continue;
            }
            let dates = recent.iter().map(|r| r.day("date")).collect::<Vec<_>>();
            if dates.iter().collect::<HashSet<_>>().len() != dates.len() {
                continue;
            }
            let Some(freq) = detect_frequency(&dates) else {
                continue;
            };
            let latest = recent.last().unwrap();
            let freshness = match freq {
                "weekly" => 14,
                "biweekly" => 28,
                "monthly" => 45,
                "quarterly" => 110,
                _ => 390,
            };
            if latest.day("date") < now - Duration::days(freshness) {
                continue;
            }
            let mut amounts = recent
                .iter()
                .map(|t| t.dec("amount").abs())
                .collect::<Vec<_>>();
            amounts.sort();
            let median = amounts[amounts.len() / 2];
            if amounts
                .iter()
                .any(|a| (*a - median).abs() > median * Decimal::new(35, 2))
            {
                continue;
            }
            suggestions.push(json!({"detection_key":key,"name":latest.s("name"),"amount":latest.dec("amount").abs().to_string(),"currency":latest.s("iso_currency_code"),"direction":if latest.dec("amount")<Decimal::ZERO{"income"}else{"expense"},"frequency":freq,"next_on":latest.s("date"),"account_id":latest["account_id"],"source_account_name":latest["source_account_name"],"merchant_key":merchant_key(latest),"history":recent,"minimum":amounts[0].to_string(),"maximum":amounts.last().unwrap().to_string()}));
        }
        suggestions.sort_by(|a, b| a.s("name").cmp(b.s("name")));
        suggestions
    }
}
pub fn signed_balance(a: &Value, value: Decimal) -> Decimal {
    if ["credit", "loan"].contains(&a.s("account_type")) {
        -value
    } else {
        value
    }
}
pub fn detection_key(t: &Value) -> Option<String> {
    Some(hash(&crate::imports::rails_json(&json!([
        merchant_key(t)?,
        t["iso_currency_code"],
        t["account_id"],
        if t.id("account_id") != 0 || t.s("source_account_name").is_empty() {
            Value::Null
        } else {
            t["source_account_name"].clone()
        }
    ]))))
}
pub fn occurrence(r: &Value, index: u32) -> NaiveDate {
    let d = r.day("next_on");
    match r.s("frequency") {
        "weekly" => d + Duration::days(7 * index as i64),
        "biweekly" => d + Duration::days(14 * index as i64),
        f => {
            let m = match f {
                "quarterly" => 3,
                "yearly" => 12,
                _ => 1,
            };
            let next = d
                .checked_add_months(Months::new(m * index))
                .unwrap_or(NaiveDate::MAX);
            if r.flag("month_end") {
                month_end(next)
            } else {
                next
            }
        }
    }
}
pub fn occurrences(r: &Value, from: NaiveDate, to: NaiveDate) -> Vec<NaiveDate> {
    if r.s("status") != "active" || from > to {
        return vec![];
    }
    let mut index = 0;
    let mut out = vec![];
    loop {
        let d = occurrence(r, index);
        if d > to {
            break;
        }
        if d >= from {
            out.push(d)
        }
        index += 1;
        if index > 100_000 {
            break;
        }
    }
    out
}
pub fn detect_frequency(dates: &[NaiveDate]) -> Option<&'static str> {
    for (name, n) in [("monthly", 1), ("quarterly", 3), ("yearly", 12)] {
        if dates.windows(2).all(|w| {
            (w[1].year() - w[0].year()) * 12 + w[1].month() as i32 - w[0].month() as i32 == n as i32
                && (w[1] - w[0].checked_add_months(Months::new(n)).unwrap())
                    .num_days()
                    .abs()
                    <= 4
        }) {
            return Some(name);
        }
    }
    for (name, n, tolerance) in [("weekly", 7, 1), ("biweekly", 14, 2)] {
        if dates
            .windows(2)
            .all(|w| ((w[1] - w[0]).num_days() - n).abs() <= tolerance)
        {
            return Some(name);
        }
    }
    None
}
#[derive(Clone)]
struct Signal {
    kind: &'static str,
    trusted: bool,
    reason: String,
}
fn signal(l: &Ledger, t: &Value) -> Option<Signal> {
    if t.flag("classification_override") && (t.flag("transfer") || t.flag("card_payment")) {
        return Some(Signal {
            kind: if t.flag("card_payment") {
                "card_payment"
            } else {
                "transfer"
            },
            trusted: true,
            reason: "Your confirmed classification".into(),
        });
    }
    let p = &t["raw_payload"]["personal_finance_category"];
    let code = p.s("detailed");
    let confidence = p.s("confidence_level");
    let kind = match code {
        "LOAN_PAYMENTS_CREDIT_CARD_PAYMENT" => Some("card_payment"),
        "TRANSFER_IN_ACCOUNT_TRANSFER" | "TRANSFER_OUT_ACCOUNT_TRANSFER" => Some("transfer"),
        _ => None,
    };
    if let Some(kind) = kind {
        return Some(Signal {
            kind,
            trusted: ["HIGH", "VERY_HIGH"].contains(&confidence),
            reason: format!("Bank category ({confidence})"),
        });
    }
    let category = if !t["raw_payload"].s("import_category").is_empty() {
        t["raw_payload"].s("import_category").to_string()
    } else {
        l.category_name(t.id("category_id"))
    };
    let leaf = category
        .rsplit(['>', ':'])
        .next()
        .unwrap_or("")
        .trim()
        .to_lowercase();
    let kind = match leaf.as_str() {
        "credit card payment" | "credit card payments" => Some("card_payment"),
        "transfer" | "transfers" => Some("transfer"),
        _ => None,
    };
    if let Some(kind) = kind {
        return Some(Signal {
            kind,
            trusted: true,
            reason: "Explicit transfer or card-payment category".into(),
        });
    }
    let text = format!("{} {}", t.s("name"), t.s("original_description"));
    let matches = |pattern: &str| pattern_match(pattern, &text);
    if matches(r"(?i)\b(?:fees?|interest|refund|reversal|cash advance|insurance|protection)\b") {
        return None;
    }
    if matches(
        r"(?i)\b(?:credit\s*card\s+(?:payment|pmt)|(?:cardmember|card\s+services)\s+(?:payment|pmt))\b",
    ) || (l
        .account(t.id("account_id"))
        .is_some_and(|a| a.s("account_type") == "credit")
        && t.dec("amount") < Decimal::ZERO
        && matches(
            r"(?i)\b(?:automatic|autopay|online|mobile|electronic)\s+(?:payment|pmt)\b|\bpayment\s+(?:received|thank\s*you)\b",
        ))
    {
        return Some(Signal {
            kind: "card_payment",
            trusted: true,
            reason: "Explicit card-payment description".into(),
        });
    }
    if matches(r"(?i)\b(?:transfer|xfer|xfr|vault)\b") {
        return Some(Signal {
            kind: "transfer",
            trusted: false,
            reason: "Transfer-like description; needs a matching owned account".into(),
        });
    }
    None
}
fn account_key(l: &Ledger, t: &Value) -> String {
    if t.id("account_id") != 0 {
        return format!("linked:{}", t.id("account_id"));
    }
    let name = normalize(t.s("source_account_name"));
    if name.is_empty() {
        return name;
    }
    let matches = l
        .accounts
        .iter()
        .filter(|a| normalize(a.s("name")) == name)
        .collect::<Vec<_>>();
    if matches.len() == 1 {
        format!("linked:{}", matches[0].id("id"))
    } else {
        format!("source:{name}")
    }
}
fn references_account(l: &Ledger, t: &Value, other: &Value) -> bool {
    let text = normalize(&format!("{} {}", t.s("name"), t.s("original_description")));
    let account = l.account(other.id("account_id"));
    let label = normalize(if !other.s("source_account_name").is_empty() {
        other.s("source_account_name")
    } else {
        account.map_or("", |a| a.s("name"))
    });
    let meaningful = regex::Regex::new(r"\b(?:account|checking|savings|credit|card|joint|bank)\b")
        .unwrap()
        .replace_all(&label, "");
    let label_match = meaningful.trim().len() >= 3
        && regex::Regex::new(&format!(
            r"(?:^|[^a-z0-9]){}(?:$|[^a-z0-9])",
            regex::escape(&label)
        ))
        .unwrap()
        .is_match(&text);
    let mask = account.map_or("", |a| a.s("mask"));
    label_match
        || (mask.len() >= 4
            && regex::Regex::new(&format!(r"(?:^|[^0-9]){}(?:$|[^0-9])", regex::escape(mask)))
                .unwrap()
                .is_match(&text))
}
pub fn classify(l: &Ledger) -> Vec<Value> {
    let rows = l
        .transactions
        .iter()
        .filter(|t| t.id("parent_id") == 0)
        .collect::<Vec<_>>();
    let signals = rows
        .iter()
        .map(|t| (t.id("id"), signal(l, t)))
        .collect::<HashMap<_, _>>();
    let matchable = |t: &Value| {
        let p = &t["raw_payload"]["personal_finance_category"];
        let explicit = signals[&t.id("id")].as_ref().is_some_and(|s| s.trusted);
        let nontransfer = ["HIGH", "VERY_HIGH"].contains(&p.s("confidence_level"))
            && !p.s("detailed").is_empty()
            && ![
                "TRANSFER_IN_ACCOUNT_TRANSFER",
                "TRANSFER_OUT_ACCOUNT_TRANSFER",
                "LOAN_PAYMENTS_CREDIT_CARD_PAYMENT",
            ]
            .contains(&p.s("detailed"))
            && !explicit;
        !t.flag("pending")
            && !t.flag("hidden")
            && !t.flag("duplicate")
            && t.dec("amount") != Decimal::ZERO
            && !t.s("iso_currency_code").is_empty()
            && !account_key(l, t).is_empty()
            && l.account(t.id("account_id"))
                .is_none_or(|a| a.s("account_type") != "loan")
            && !nontransfer
    };
    let mut amounts = HashMap::<(String, Decimal), Vec<&Value>>::new();
    for t in &rows {
        if matchable(t) {
            amounts
                .entry((t.s("iso_currency_code").into(), t.dec("amount")))
                .or_default()
                .push(t)
        }
    }
    let mut choices = HashMap::<i64, i64>::new();
    for t in &rows {
        if !matchable(t) {
            continue;
        }
        let candidates = amounts
            .get(&(t.s("iso_currency_code").into(), -t.dec("amount")))
            .cloned()
            .unwrap_or_default();
        let mut ranked = vec![];
        for other in candidates {
            if account_key(l, t) == account_key(l, other)
                || (t.day("date") - other.day("date")).num_days().abs() > 3
                || [t, &other].iter().any(|r| {
                    r.flag("classification_override")
                        && !r.flag("transfer")
                        && !r.flag("card_payment")
                })
            {
                continue;
            }
            let a = &signals[&t.id("id")];
            let b = &signals[&other.id("id")];
            if a.is_none() && b.is_none() {
                continue;
            }
            let named = references_account(l, t, other) || references_account(l, other, t);
            let consistent = a.as_ref().zip(b.as_ref()).is_some_and(|(a, b)| {
                a.kind == b.kind || a.kind == "card_payment" || b.kind == "card_payment"
            });
            if !named && !consistent {
                continue;
            }
            let (outgoing, incoming) = if t.dec("amount") > Decimal::ZERO {
                (*t, other)
            } else {
                (other, *t)
            };
            let rank = (
                (references_account(l, outgoing, incoming) as i32) * 2
                    + references_account(l, incoming, outgoing) as i32,
                -(t.day("date") - other.day("date")).num_days().abs(),
            );
            ranked.push((rank, other.id("id")));
        }
        ranked.sort_by(|a, b| b.0.cmp(&a.0));
        if !ranked.is_empty() && (ranked.len() == 1 || ranked[0].0 != ranked[1].0) {
            choices.insert(t.id("id"), ranked[0].1);
        }
    }
    rows.into_iter().filter_map(|t|{if t.flag("classification_override")||(t.s("classification_origin")!="automatic"&&(t.flag("transfer")||t.flag("card_payment"))){return None}let evidence=signals[&t.id("id")].as_ref();let pair=choices.get(&t.id("id")).filter(|other|choices.get(other)==Some(&t.id("id")));let mut kind=None;let mut reason=None;
        if let Some(other)=pair{let other_t=l.transaction(*other).unwrap();let card=evidence.is_some_and(|s|s.kind=="card_payment")||signals[other].as_ref().is_some_and(|s|s.kind=="card_payment")||[t,other_t].iter().any(|t|l.account(t.id("account_id")).is_some_and(|a|a.s("account_type")=="credit"));kind=Some(if card{"card_payment"}else{"transfer"});reason=Some("Opposite amounts in different owned accounts within 3 days, corroborated by transfer/payment details".to_string());}else if let Some(e)=evidence.filter(|e|e.trusted){kind=Some(e.kind);reason=Some(e.reason.clone())}
        let mut v=t.clone();v["transfer"]=json!(kind==Some("transfer"));v["card_payment"]=json!(kind==Some("card_payment"));v["classification_origin"]=json!(kind.map(|_|"automatic"));v["classification_reason"]=json!(reason);v["matched_transaction_id"]=json!(pair);v["suggested_classification"]=json!(if kind.is_none(){evidence.map(|e|e.kind)}else{None});v["suggestion_reason"]=json!(if kind.is_none(){evidence.map(|e|&e.reason)}else{None});Some(v)
    }).collect()
}
pub fn categorize(l: &Ledger) -> Vec<Value> {
    let mappings: Value = serde_json::from_str(include_str!("bank_categories.json")).unwrap();
    let mut changed = vec![];
    let category_for = |path: &Value| -> Option<i64> {
        let parts = path.as_array()?;
        let name = parts
            .iter()
            .map(|v| v.as_str().unwrap_or(""))
            .collect::<Vec<_>>()
            .join(" > ")
            .to_lowercase();
        let matches = l
            .categories
            .iter()
            .filter(|c| l.category_name(c.id("id")).to_lowercase() == name)
            .collect::<Vec<_>>();
        let personal = matches
            .iter()
            .filter(|c| c.id("user_id") == l.user)
            .collect::<Vec<_>>();
        if personal.len() == 1 {
            Some(personal[0].id("id"))
        } else if personal.is_empty() && matches.len() == 1 {
            Some(matches[0].id("id"))
        } else {
            None
        }
    };
    let history = l
        .transactions
        .iter()
        .filter(|t| {
            t.id("parent_id") == 0
                && !excluded(t)
                && !t.flag("is_split")
                && t.flag("category_locked")
                && t.s("category_origin") == "manual"
                && t.id("category_id") != 0
        })
        .collect::<Vec<_>>();
    for t in &l.transactions {
        if t.id("parent_id") != 0
            || excluded(t)
            || t.flag("is_split")
            || t.flag("category_locked")
            || (t.id("category_id") != 0 && t.s("category_origin").is_empty())
        {
            continue;
        }
        let mut category = None;
        let mut origin = None;
        let mut reason = None;
        let mut suggestion = None;
        let mut suggestion_reason = None;
        if let Some(rule) = l.rules.iter().rev().find(|r| {
            r.id("category_id") != 0
                && l.category(r.id("category_id")).is_some()
                && rule_matches(r, t)
                && (!r.s("merchant_key").is_empty()
                    || if r.s("effective_from").is_empty() {
                        r.s("created_at")
                    } else {
                        r.s("effective_from")
                    } <= t.s("created_at"))
        }) {
            category = Some(rule.id("category_id"));
            origin = Some("rule");
            reason = Some("Your saved rule".into())
        } else if let Some(key) = merchant_key(t) {
            let known = history
                .iter()
                .filter(|row| {
                    row.id("id") != t.id("id") && merchant_key(row).as_deref() == Some(&key)
                })
                .collect::<Vec<_>>();
            if known.len() >= 2
                && known
                    .iter()
                    .all(|r| r.id("category_id") == known[0].id("category_id"))
            {
                category = Some(known[0].id("category_id"));
                origin = Some("merchant_history");
                reason = Some(format!(
                    "Your consistent category on {} previous transactions from this merchant",
                    known.len()
                ));
            }
        }
        if category.is_none() {
            let bank = &t["raw_payload"]["personal_finance_category"];
            let code = bank.s("detailed");
            if !code.is_empty()
                && !code.starts_with("TRANSFER_")
                && !code.starts_with("LOAN_DISBURSEMENTS_")
                && code != "LOAN_PAYMENTS_CREDIT_CARD_PAYMENT"
            {
                let mut primary = bank.s("primary").to_string();
                if primary.is_empty() {
                    let mut keys = mappings["PRIMARY_PATHS"]
                        .as_object()
                        .unwrap()
                        .keys()
                        .collect::<Vec<_>>();
                    keys.sort_by_key(|k| std::cmp::Reverse(k.len()));
                    primary = keys
                        .into_iter()
                        .find(|k| code.starts_with(&format!("{k}_")))
                        .cloned()
                        .unwrap_or_default()
                }
                let detail = &mappings["DETAIL_PATHS"][code];
                let mut mapped = category_for(detail);
                if mapped.is_none() && detail.as_array().is_some_and(|a| a.len() == 2) {
                    mapped = category_for(&json!([detail[0]]))
                }
                if mapped.is_none() {
                    mapped = category_for(&mappings["PRIMARY_PATHS"][&primary])
                }
                if let Some(id) = mapped {
                    if ["HIGH", "VERY_HIGH"].contains(&bank.s("confidence_level")) {
                        category = Some(id);
                        origin = Some("provider");
                        reason = Some(format!(
                            "Bank category: {} ({} confidence)",
                            l.category_name(id),
                            bank.s("confidence_level")
                        ))
                    } else {
                        suggestion = Some(id);
                        suggestion_reason = Some(format!(
                            "Bank suggests {}; confidence needs review",
                            l.category_name(id)
                        ))
                    }
                } else {
                    suggestion_reason = Some(format!(
                        "No matching existing category for {code}; choose a category or add a rule"
                    ))
                }
            }
        }
        let mut v = t.clone();
        v["category_id"] = json!(category);
        v["category_origin"] = json!(origin);
        v["category_reason"] = json!(reason);
        v["category_locked"] = json!(origin == Some("rule"));
        v["suggested_category_id"] = json!(suggestion);
        v["category_suggestion_reason"] = json!(suggestion_reason);
        changed.push(v)
    }
    changed
}
pub async fn refresh(conn: &mut PgConnection, user: i64) -> Result<()> {
    let mut l = Ledger::load(conn, user).await?;
    for row in classify(&l) {
        let id = row.id("id");
        if l.transaction(id) != Some(&row) {
            db::save(conn, "transactions", user, Some(id), row.clone()).await?;
            if let Some(t) = l.transactions.iter_mut().find(|t| t.id("id") == id) {
                *t = row
            }
        }
    }
    for row in categorize(&l) {
        let id = row.id("id");
        if l.transaction(id) != Some(&row) {
            db::save(conn, "transactions", user, Some(id), row).await?;
        }
    }
    Ok(())
}

fn pattern_match(pattern: &str, text: &str) -> bool {
    use std::sync::{LazyLock, RwLock};
    static CACHE: LazyLock<RwLock<HashMap<String, regex::Regex>>> =
        LazyLock::new(|| RwLock::new(HashMap::new()));
    if let Some(regex) = CACHE.read().unwrap().get(pattern) {
        return regex.is_match(text);
    }
    let regex = regex::Regex::new(pattern).expect("Internal regex is valid");
    let matched = regex.is_match(text);
    CACHE.write().unwrap().insert(pattern.to_string(), regex);
    matched
}
