# Comparing the implementations

The starting point is the Rails working directory on 2026-11-25, including its
uncommitted workflows. Its Git HEAD was `db0b44693634ee6e51dd6b1f40227acd538634a3`.
`rails-source-manifest.json` records SHA-256 hashes of those exact source files.
No Rails source files are included as a competing runtime or modified by this port.

| Rails source | Rust implementation |
|---|---|
| `config/routes.rb`, controllers | `src/web.rs`, `src/actions.rs`, `src/views.rs` |
| Active Record schema and models | `migrations/`, `src/db.rs`, `src/domain.rs` |
| CashFlowClassifier, AutoCategorizer | `src/domain.rs` |
| FinancialReport, ExpenseClassification, TransactionSearch | `src/domain.rs` |
| RecurringItem and RecurringDetector | `src/domain.rs` |
| CSV and Tiller services | `src/imports.rs` |
| Plaid services and Active Job jobs | `src/plaid.rs` |
| ERB views and helpers | `templates/`, `src/views.rs`, `src/web.rs` |
| Styles, Turbo, Android shell | `static/`, `android/` |

The database preserves Rails field names, IDs, precision and constraints to make
comparison practical. SQLx binds all record values; dynamic table/column identifiers
are checked against the embedded schema. Domain services use `rust_decimal` and
`chrono`, with JSON records at the database/import boundary.

## Intentional differences

- Separate database, cookie and port; Rust tokens have a new authenticated encryption
  format. A copied Rails session is not reused.
- Persisted PostgreSQL jobs replace the source's in-process development job adapter.
  The server handles the worker; no Redis or separate worker process is needed.
- MiniJinja templates replace Rails helpers. The familiar navigation, colors and
  responsive shell remain. Reports use bars/tables and the same financial scopes;
  the markup and chart presentation are not pixel-for-pixel ERB copies.
- HTML forms use POST with method overrides for removals; actions also accept the
  original PATCH-style requests. Turbo accelerates normal page navigation.
- Imports refuse overwriting an existing Rust ledger. The migration is a snapshot,
  not bidirectional synchronization.
- Manual asset currency is immutable after valuations exist, preventing past values
  from being silently relabeled as another currency.

## Verification

- Decimal arithmetic, exclusion inheritance, rules, categorization, transfer matching,
  recurring dates, budget rollover, net-worth coverage, encrypted token integrity,
  duplicate posting windows and template syntax have unit coverage.
- The synthetic Tiller fixture is run through both importers. Transactions, source
  fingerprints, account overrides, categories, cached balances and rule metadata are
  compared with the saved Rails reference in `tests/fixtures/`.
- The HTTP/PostgreSQL workflow test exercises signup/sign-in, CSRF, cross-user access,
  escaped user content, transactions, splits, budgets, assets, recurring state, rules,
  CSV/Tiller imports and reimports, pending-to-posted bank updates, reconciliation,
  account tracking, queue deduplication, exports and session invalidation.
- `bin/import-rails` verifies monthly financial totals before committing a data copy.
  `bin/compare-rails` independently compares report totals and every net-worth point.
- Android unit tests, lint and the debug APK build passed against the port-3001 wrapper.
- Desktop and 390px-wide browser checks covered sign-in, navigation, search, responsive
  layout and sign-out using a temporary synthetic account.

Live Plaid Link/OAuth and actual bank refreshes require the configured provider and
have not been exercised against a real bank as part of this port. Docker and a fresh
Android installation are separate from the Rust unit/database checks; see the Android
README for its build/check commands. This repository is for side-by-side evaluation;
passing the current comparisons does not prove every possible input has identical
behavior to Rails.
