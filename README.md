# Worth

A Rust implementation of Worth, the personal finance app. The repository is named
`worth-rust` to distinguish it from the Rails implementation; the product name,
wordmark, page titles, and bank-link client name remain **Worth**.

The backend runs on Axum, SQLx/PostgreSQL, and Tokio. MiniJinja renders the HTML;
Turbo and the existing Worth CSS keep the web and Hotwire Native navigation model.
Amounts use decimal arithmetic throughout the ledger. There is no Ruby dependency
at runtime; Ruby is used only by the optional one-time Rails comparison/import tools.

## Run beside Rails

Prerequisites: Rust 1.88, PostgreSQL 16+, and the `psql`/`createdb` command-line tools.

```sh
bin/setup
bin/dev
```

Open **http://localhost:3001**. Keep Rails on **http://localhost:3000**.

| | Rails | Rust |
|---|---|---|
| Local directory | `Budget` | `worth-rust` |
| Default port | 3000 | 3001 |
| Local database | `budget_development` | `worth_rust_development` |
| Session cookie | `_budget_session` | `_worth_rust_session` |
| App name | Worth | Worth |

The applications use independent databases. Changes and refreshes in one app do
not update the other. Do not configure them to write to the same database. Rust
rejects a `DATABASE_URL` whose database name does not start with `worth_rust_`.

`bin/setup` generates a local encryption key and a private `.env`. For a remote
PostgreSQL server, create your `worth_rust_*` database yourself, copy `.env.example`
to `.env`, set `DATABASE_URL` and `WORTH_ENCRYPTION_KEY`, then use `bin/dev`.
The app applies embedded SQL migrations at startup. Back up the database and its
encryption key together; losing the key makes bank tokens unreadable.

To use Docker, generate `.env` first (`cp .env.example .env` and set an encryption
key with `openssl rand -base64 32`), then run `docker compose up --build`.
Docker uses its own named database volume and host port 5433 for PostgreSQL.

## Copy your Rails data

Start with an empty Rust database:

```sh
bin/import-rails /absolute/path/to/Budget
# Optionally also copy Plaid credentials into the ignored local .env:
bin/import-rails /absolute/path/to/Budget --copy-plaid-config
```

Choose one command; the importer refuses to overwrite a populated ledger. Run it
with the Rust server stopped and restart afterward if configuration was copied.
The source is read in a read-only, repeatable-read transaction. Records and IDs,
password hashes, manual choices, imports, splits, valuations, and balances are
preserved. Bank tokens are decrypted in the Rails process, piped directly to Rust,
and re-encrypted with AES-256-GCM under the new local key, bound to their owner and
Plaid Item. Plaintext token exports are never written to disk. Sessions and queued
refreshes are not copied. No bank API is called by the migration.

The importer compares every month's spending, income, and reportable record count
before committing. Use your existing email and password in Rust; sign in separately.
You can also perform a read-only comparison of total cash flow and every saved
net-worth observation:

```sh
bin/compare-rails /absolute/path/to/Budget
```

These comparisons are expected to diverge after you intentionally edit either app.

## Included workflows

- Accounts, tracking choices, current balances, manual assets, and valuation history.
- Transactions: search, combined filters, pagination, CSV export, bulk edits, manual
  entries, notes, tags, category locks, duplicate review, splits and reconciliation.
- Personal and shared categories, subtree merges, expense preferences, description
  and account rules, merchant preferences, deterministic automatic categorization.
- Cash-flow classification with protected manual choices and corroborated transfers.
- Category budgets with rollover, income/spending reports, fixed/variable expenses,
  and historical net worth with coverage and currency separation.
- Recurring bills, subscriptions, income, history-based suggestions, dismiss/restore,
  pause/resume, anchored month-end dates, and a 30-day outlook.
- CSV mapping, reusable mapping profiles, explicit duplicate selection and reimports.
- Tiller XLSX previews: cached values only, source-account mapping, asset valuations,
  categories, source-ID conflicts, duplicate flags, daily balances, import receipts,
  and AutoCat rules retained for reference.
- Plaid Link, account updates, transaction cursor sync, pending-to-posted reconciliation,
  encrypted tokens, and a PostgreSQL refresh queue with leases and bounded retries.
- Independent server-side sessions, CSRF protection, password-change invalidation,
  escaped templates, and user-scoped records.
- An Android Hotwire Native wrapper using the same pages. See [android/README.md](android/README.md).

See [docs/comparison.md](docs/comparison.md) for source mapping, validation and
intentional differences. The HTTP paths largely follow Rails; this is a web app,
not a complete public REST API. Only the Plaid endpoints are JSON APIs.

## Bank configuration

Set `PLAID_CLIENT_ID`, `PLAID_SECRET`, and `PLAID_ENV` (`sandbox`, `development`, or
`production`) in `.env`. Optional settings: `PLAID_PRODUCTS`, `PLAID_COUNTRY_CODES`,
`PLAID_TRANSACTIONS_DAYS_REQUESTED`, and `PLAID_REDIRECT_URI`. The displayed client
name is always Worth. Plaid's hosted Link JavaScript is loaded only when connecting
a bank. The app does not call an AI service or execute spreadsheet formulas.

Refreshes run only after bank linking or an explicit refresh request. Imported
connections are not automatically refreshed. Jobs survive restarts and are retried
up to five times; only one job per connection is queued. Existing account tracking
choices and transaction edits are retained during upserts.

## Tests and checks

```sh
bin/test
```

The integration test creates and drops a randomly named `worth_rust_test_*` database.
It never uses your application databases. Set `WORTH_TEST_ADMIN_URL` if your local
PostgreSQL admin connection differs from `postgresql://localhost/postgres`; the test
role needs `CREATEDB`. A failed test may leave its uniquely named test database for
diagnosis. Unit tests can run without PostgreSQL using `cargo test --test domain`.

GitHub Actions runs formatting, Clippy with warnings denied, unit/HTTP/database tests,
and a release build. Tests use synthetic fixtures and never contact real banks.

Run the server from the repository root, or set `WORTH_ASSET_ROOT` to the directory
containing `templates/` and `static/`. For production, use TLS, `COOKIE_SECURE=true`,
a private database connection, and a securely stored encryption key. Deployment,
Play Store release, push notifications, offline operation, bill payment, investment
analytics, and FX conversion are outside this implementation, as in the source app.
