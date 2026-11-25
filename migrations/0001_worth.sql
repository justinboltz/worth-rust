-- Translated from the Worth Rails working schema, preserving IDs and precision.

CREATE TABLE "accounts" (
  id BIGSERIAL PRIMARY KEY,
  "user_id" BIGINT NOT NULL,
  "plaid_item_id" BIGINT,
  "plaid_account_id" TEXT,
  "name" TEXT NOT NULL,
  "official_name" TEXT,
  "account_type" TEXT NOT NULL,
  "subtype" TEXT,
  "mask" TEXT,
  "iso_currency_code" TEXT,
  "current_balance" NUMERIC(19,4),
  "available_balance" NUMERIC(19,4),
  "limit_balance" NUMERIC(19,4),
  "created_at" TIMESTAMP(6) DEFAULT CURRENT_TIMESTAMP NOT NULL,
  "updated_at" TIMESTAMP(6) DEFAULT CURRENT_TIMESTAMP NOT NULL,
  "tracked" BOOLEAN DEFAULT true NOT NULL,
  "import_source_key" TEXT,
  "import_metadata" JSONB DEFAULT '{}'::jsonb NOT NULL,
  "net_worth_excluded" BOOLEAN DEFAULT false NOT NULL
);

CREATE TABLE "asset_valuations" (
  id BIGSERIAL PRIMARY KEY,
  "manual_asset_id" BIGINT NOT NULL,
  "value" NUMERIC(19,2) NOT NULL,
  "valued_on" DATE NOT NULL,
  "created_at" TIMESTAMP(6) DEFAULT CURRENT_TIMESTAMP NOT NULL,
  "updated_at" TIMESTAMP(6) DEFAULT CURRENT_TIMESTAMP NOT NULL
);

CREATE TABLE "balance_snapshots" (
  id BIGSERIAL PRIMARY KEY,
  "user_id" BIGINT NOT NULL,
  "account_id" BIGINT NOT NULL,
  "as_of_date" DATE NOT NULL,
  "current_balance" NUMERIC(19,4),
  "available_balance" NUMERIC(19,4),
  "iso_currency_code" TEXT,
  "created_at" TIMESTAMP(6) DEFAULT CURRENT_TIMESTAMP NOT NULL,
  "updated_at" TIMESTAMP(6) DEFAULT CURRENT_TIMESTAMP NOT NULL,
  "import_metadata" JSONB DEFAULT '{}'::jsonb NOT NULL
);

CREATE TABLE "budgets" (
  id BIGSERIAL PRIMARY KEY,
  "user_id" BIGINT NOT NULL,
  "category_id" BIGINT NOT NULL,
  "amount" NUMERIC(19,2) NOT NULL,
  "currency" TEXT DEFAULT 'USD' NOT NULL,
  "starts_on" DATE NOT NULL,
  "rollover" BOOLEAN DEFAULT false NOT NULL,
  "created_at" TIMESTAMP(6) DEFAULT CURRENT_TIMESTAMP NOT NULL,
  "updated_at" TIMESTAMP(6) DEFAULT CURRENT_TIMESTAMP NOT NULL
);

CREATE TABLE "categories" (
  id BIGSERIAL PRIMARY KEY,
  "user_id" BIGINT,
  "parent_id" BIGINT,
  "name" TEXT NOT NULL,
  "system" BOOLEAN DEFAULT false NOT NULL,
  "created_at" TIMESTAMP(6) DEFAULT CURRENT_TIMESTAMP NOT NULL,
  "updated_at" TIMESTAMP(6) DEFAULT CURRENT_TIMESTAMP NOT NULL
);

CREATE TABLE "category_expense_types" (
  id BIGSERIAL PRIMARY KEY,
  "user_id" BIGINT NOT NULL,
  "category_id" BIGINT NOT NULL,
  "expense_type" TEXT NOT NULL,
  "created_at" TIMESTAMP(6) DEFAULT CURRENT_TIMESTAMP NOT NULL,
  "updated_at" TIMESTAMP(6) DEFAULT CURRENT_TIMESTAMP NOT NULL
);

CREATE TABLE "import_batches" (
  id BIGSERIAL PRIMARY KEY,
  "user_id" BIGINT NOT NULL,
  "csv_text" TEXT NOT NULL,
  "settings" JSONB DEFAULT '{}'::jsonb NOT NULL,
  "imported_at" TIMESTAMP(6),
  "created_at" TIMESTAMP(6) DEFAULT CURRENT_TIMESTAMP NOT NULL,
  "updated_at" TIMESTAMP(6) DEFAULT CURRENT_TIMESTAMP NOT NULL
);

CREATE TABLE "import_profiles" (
  id BIGSERIAL PRIMARY KEY,
  "user_id" BIGINT NOT NULL,
  "name" TEXT NOT NULL,
  "settings" JSONB DEFAULT '{}'::jsonb NOT NULL,
  "created_at" TIMESTAMP(6) DEFAULT CURRENT_TIMESTAMP NOT NULL,
  "updated_at" TIMESTAMP(6) DEFAULT CURRENT_TIMESTAMP NOT NULL
);

CREATE TABLE "institutions" (
  id BIGSERIAL PRIMARY KEY,
  "name" TEXT NOT NULL,
  "plaid_institution_id" TEXT NOT NULL,
  "created_at" TIMESTAMP(6) DEFAULT CURRENT_TIMESTAMP NOT NULL,
  "updated_at" TIMESTAMP(6) DEFAULT CURRENT_TIMESTAMP NOT NULL
);

CREATE TABLE "manual_assets" (
  id BIGSERIAL PRIMARY KEY,
  "user_id" BIGINT NOT NULL,
  "name" TEXT NOT NULL,
  "asset_type" TEXT NOT NULL,
  "value" NUMERIC(19,2) NOT NULL,
  "currency" TEXT DEFAULT 'USD' NOT NULL,
  "valued_on" DATE NOT NULL,
  "notes" TEXT,
  "created_at" TIMESTAMP(6) DEFAULT CURRENT_TIMESTAMP NOT NULL,
  "updated_at" TIMESTAMP(6) DEFAULT CURRENT_TIMESTAMP NOT NULL
);

CREATE TABLE "plaid_items" (
  id BIGSERIAL PRIMARY KEY,
  "user_id" BIGINT NOT NULL,
  "institution_id" BIGINT NOT NULL,
  "plaid_item_id" TEXT NOT NULL,
  "access_token" TEXT NOT NULL,
  "cursor" TEXT,
  "status" TEXT DEFAULT 'active' NOT NULL,
  "last_synced_at" TIMESTAMP(6),
  "created_at" TIMESTAMP(6) DEFAULT CURRENT_TIMESTAMP NOT NULL,
  "updated_at" TIMESTAMP(6) DEFAULT CURRENT_TIMESTAMP NOT NULL,
  "sync_requested_at" TIMESTAMP(6),
  "sync_request_token" TEXT
);

CREATE TABLE "recurring_dismissals" (
  id BIGSERIAL PRIMARY KEY,
  "user_id" BIGINT NOT NULL,
  "detection_key" TEXT NOT NULL,
  "name" TEXT NOT NULL,
  "created_at" TIMESTAMP(6) DEFAULT CURRENT_TIMESTAMP NOT NULL,
  "updated_at" TIMESTAMP(6) DEFAULT CURRENT_TIMESTAMP NOT NULL
);

CREATE TABLE "recurring_items" (
  id BIGSERIAL PRIMARY KEY,
  "user_id" BIGINT NOT NULL,
  "account_id" BIGINT,
  "name" TEXT NOT NULL,
  "amount" NUMERIC(14,4) NOT NULL,
  "currency" TEXT DEFAULT 'USD' NOT NULL,
  "direction" TEXT DEFAULT 'expense' NOT NULL,
  "frequency" TEXT DEFAULT 'monthly' NOT NULL,
  "next_on" DATE NOT NULL,
  "month_end" BOOLEAN DEFAULT false NOT NULL,
  "status" TEXT DEFAULT 'active' NOT NULL,
  "merchant_key" TEXT,
  "source_account_name" TEXT,
  "detection_key" TEXT,
  "notes" TEXT,
  "created_at" TIMESTAMP(6) DEFAULT CURRENT_TIMESTAMP NOT NULL,
  "updated_at" TIMESTAMP(6) DEFAULT CURRENT_TIMESTAMP NOT NULL
);

CREATE TABLE "transaction_rules" (
  id BIGSERIAL PRIMARY KEY,
  "user_id" BIGINT NOT NULL,
  "category_id" BIGINT,
  "match_text" TEXT,
  "kind" TEXT DEFAULT 'regular' NOT NULL,
  "tags" TEXT DEFAULT '' NOT NULL,
  "created_at" TIMESTAMP(6) DEFAULT CURRENT_TIMESTAMP NOT NULL,
  "updated_at" TIMESTAMP(6) DEFAULT CURRENT_TIMESTAMP NOT NULL,
  "merchant_key" TEXT,
  "account_id" BIGINT,
  "source_account_name" TEXT,
  "description_operator" TEXT DEFAULT 'contains' NOT NULL,
  "effective_from" TIMESTAMP(6)
);

CREATE TABLE "transactions" (
  id BIGSERIAL PRIMARY KEY,
  "user_id" BIGINT NOT NULL,
  "account_id" BIGINT,
  "plaid_item_id" BIGINT,
  "category_id" BIGINT,
  "plaid_transaction_id" TEXT,
  "source" TEXT DEFAULT 'plaid' NOT NULL,
  "source_transaction_id" TEXT,
  "pending" BOOLEAN DEFAULT false NOT NULL,
  "amount" NUMERIC(19,4) NOT NULL,
  "iso_currency_code" TEXT,
  "date" DATE NOT NULL,
  "authorized_date" DATE,
  "name" TEXT NOT NULL,
  "merchant_name" TEXT,
  "merchant_entity_id" TEXT,
  "payment_channel" TEXT,
  "pending_transaction_id" TEXT,
  "original_description" TEXT,
  "notes" TEXT,
  "raw_payload" JSONB,
  "created_at" TIMESTAMP(6) DEFAULT CURRENT_TIMESTAMP NOT NULL,
  "updated_at" TIMESTAMP(6) DEFAULT CURRENT_TIMESTAMP NOT NULL,
  "source_account_name" TEXT,
  "tags" TEXT DEFAULT '' NOT NULL,
  "hidden" BOOLEAN DEFAULT false NOT NULL,
  "duplicate" BOOLEAN DEFAULT false NOT NULL,
  "transfer" BOOLEAN DEFAULT false NOT NULL,
  "card_payment" BOOLEAN DEFAULT false NOT NULL,
  "is_split" BOOLEAN DEFAULT false NOT NULL,
  "parent_id" BIGINT,
  "split_needs_review" BOOLEAN DEFAULT false NOT NULL,
  "classification_override" BOOLEAN DEFAULT false NOT NULL,
  "classification_origin" TEXT,
  "classification_reason" TEXT,
  "suggested_classification" TEXT,
  "suggestion_reason" TEXT,
  "matched_transaction_id" BIGINT,
  "category_locked" BOOLEAN DEFAULT false NOT NULL,
  "category_origin" TEXT,
  "category_reason" TEXT,
  "category_suggestion_reason" TEXT,
  "suggested_category_id" BIGINT,
  "expense_type" TEXT
);

CREATE TABLE "users" (
  id BIGSERIAL PRIMARY KEY,
  "email" TEXT NOT NULL,
  "password_digest" TEXT NOT NULL,
  "created_at" TIMESTAMP(6) DEFAULT CURRENT_TIMESTAMP NOT NULL,
  "updated_at" TIMESTAMP(6) DEFAULT CURRENT_TIMESTAMP NOT NULL,
  "session_token" TEXT NOT NULL
);

CREATE UNIQUE INDEX "index_accounts_on_plaid_account_id" ON "accounts" ("plaid_account_id");

CREATE INDEX "index_accounts_on_plaid_item_id" ON "accounts" ("plaid_item_id");

CREATE UNIQUE INDEX "index_accounts_on_user_id_and_import_source_key" ON "accounts" ("user_id", "import_source_key") WHERE (import_source_key IS NOT NULL);

CREATE INDEX "index_accounts_on_user_id" ON "accounts" ("user_id");

CREATE UNIQUE INDEX "index_asset_valuations_on_manual_asset_id_and_valued_on" ON "asset_valuations" ("manual_asset_id", "valued_on");

CREATE INDEX "index_asset_valuations_on_manual_asset_id" ON "asset_valuations" ("manual_asset_id");

CREATE UNIQUE INDEX "index_balance_snapshots_on_account_id_and_as_of_date" ON "balance_snapshots" ("account_id", "as_of_date");

CREATE INDEX "index_balance_snapshots_on_account_id" ON "balance_snapshots" ("account_id");

CREATE INDEX "index_balance_snapshots_on_user_id" ON "balance_snapshots" ("user_id");

CREATE INDEX "index_budgets_on_category_id" ON "budgets" ("category_id");

CREATE UNIQUE INDEX "index_budgets_on_user_id_and_category_id" ON "budgets" ("user_id", "category_id");

CREATE INDEX "index_budgets_on_user_id" ON "budgets" ("user_id");

CREATE UNIQUE INDEX "index_categories_on_parent_id_and_name" ON "categories" ("parent_id", "name") WHERE (user_id IS NULL);

CREATE INDEX "index_categories_on_parent_id" ON "categories" ("parent_id");

CREATE UNIQUE INDEX "index_categories_on_user_id_and_parent_id_and_name" ON "categories" ("user_id", "parent_id", "name");

CREATE INDEX "index_categories_on_user_id" ON "categories" ("user_id");

CREATE INDEX "index_category_expense_types_on_category_id" ON "category_expense_types" ("category_id");

CREATE UNIQUE INDEX "index_category_expense_types_on_user_id_and_category_id" ON "category_expense_types" ("user_id", "category_id");

CREATE INDEX "index_category_expense_types_on_user_id" ON "category_expense_types" ("user_id");

CREATE INDEX "index_import_batches_on_user_id" ON "import_batches" ("user_id");

CREATE INDEX "index_import_profiles_on_user_id" ON "import_profiles" ("user_id");

CREATE UNIQUE INDEX "index_institutions_on_plaid_institution_id" ON "institutions" ("plaid_institution_id");

CREATE INDEX "index_manual_assets_on_user_id" ON "manual_assets" ("user_id");

CREATE INDEX "index_plaid_items_on_institution_id" ON "plaid_items" ("institution_id");

CREATE UNIQUE INDEX "index_plaid_items_on_plaid_item_id" ON "plaid_items" ("plaid_item_id");

CREATE INDEX "index_plaid_items_on_user_id" ON "plaid_items" ("user_id");

CREATE UNIQUE INDEX "index_recurring_dismissals_on_user_id_and_detection_key" ON "recurring_dismissals" ("user_id", "detection_key");

CREATE INDEX "index_recurring_dismissals_on_user_id" ON "recurring_dismissals" ("user_id");

CREATE INDEX "index_recurring_items_on_account_id" ON "recurring_items" ("account_id");

CREATE UNIQUE INDEX "index_recurring_items_on_user_id_and_detection_key" ON "recurring_items" ("user_id", "detection_key");

CREATE INDEX "index_recurring_items_on_user_id" ON "recurring_items" ("user_id");

CREATE INDEX "index_transaction_rules_on_account_id" ON "transaction_rules" ("account_id");

CREATE INDEX "index_transaction_rules_on_category_id" ON "transaction_rules" ("category_id");

CREATE UNIQUE INDEX "index_transaction_rules_on_user_id_and_merchant_key" ON "transaction_rules" ("user_id", "merchant_key") WHERE (merchant_key IS NOT NULL);

CREATE INDEX "index_transaction_rules_on_user_id" ON "transaction_rules" ("user_id");

CREATE INDEX "index_transactions_on_account_id_and_date" ON "transactions" ("account_id", "date");

CREATE INDEX "index_transactions_on_account_id" ON "transactions" ("account_id");

CREATE INDEX "index_transactions_on_category_id" ON "transactions" ("category_id");

CREATE INDEX "index_transactions_on_matched_transaction_id" ON "transactions" ("matched_transaction_id");

CREATE INDEX "index_transactions_on_parent_id" ON "transactions" ("parent_id");

CREATE INDEX "index_transactions_on_plaid_item_id" ON "transactions" ("plaid_item_id");

CREATE INDEX "index_transactions_on_suggested_category_id" ON "transactions" ("suggested_category_id");

CREATE UNIQUE INDEX "index_transactions_on_user_id_and_plaid_transaction_id" ON "transactions" ("user_id", "plaid_transaction_id") WHERE (plaid_transaction_id IS NOT NULL);

CREATE UNIQUE INDEX "idx_on_user_id_source_source_transaction_id_9233d84c22" ON "transactions" ("user_id", "source", "source_transaction_id") WHERE (source_transaction_id IS NOT NULL);

CREATE INDEX "index_transactions_on_user_id" ON "transactions" ("user_id");

CREATE UNIQUE INDEX "index_users_on_email" ON "users" ("email");

CREATE UNIQUE INDEX "index_users_on_session_token" ON "users" ("session_token");

ALTER TABLE "accounts" ADD CONSTRAINT "accounts_have_a_source" CHECK (plaid_item_id IS NOT NULL AND plaid_account_id IS NOT NULL OR import_source_key IS NOT NULL);

ALTER TABLE "category_expense_types" ADD CONSTRAINT "valid_category_expense_type" CHECK (expense_type::text = ANY (ARRAY['fixed'::character varying, 'variable'::character varying, 'unclassified'::character varying]::text[]));

ALTER TABLE "recurring_items" ADD CONSTRAINT "recurring_amount_positive" CHECK (amount > 0::numeric);

ALTER TABLE "transactions" ADD CONSTRAINT "valid_transaction_expense_type" CHECK (expense_type IS NULL OR (expense_type::text = ANY (ARRAY['fixed'::character varying, 'variable'::character varying, 'unclassified'::character varying]::text[])));

ALTER TABLE "accounts" ADD FOREIGN KEY ("plaid_item_id") REFERENCES "plaid_items"(id);

ALTER TABLE "accounts" ADD FOREIGN KEY ("user_id") REFERENCES "users"(id);

ALTER TABLE "asset_valuations" ADD FOREIGN KEY ("manual_asset_id") REFERENCES "manual_assets"(id);

ALTER TABLE "balance_snapshots" ADD FOREIGN KEY ("account_id") REFERENCES "accounts"(id);

ALTER TABLE "balance_snapshots" ADD FOREIGN KEY ("user_id") REFERENCES "users"(id);

ALTER TABLE "budgets" ADD FOREIGN KEY ("category_id") REFERENCES "categories"(id);

ALTER TABLE "budgets" ADD FOREIGN KEY ("user_id") REFERENCES "users"(id);

ALTER TABLE "categories" ADD FOREIGN KEY ("parent_id") REFERENCES "categories"(id);

ALTER TABLE "categories" ADD FOREIGN KEY ("user_id") REFERENCES "users"(id);

ALTER TABLE "category_expense_types" ADD FOREIGN KEY ("category_id") REFERENCES "categories"(id) ON DELETE CASCADE;

ALTER TABLE "category_expense_types" ADD FOREIGN KEY ("user_id") REFERENCES "users"(id);

ALTER TABLE "import_batches" ADD FOREIGN KEY ("user_id") REFERENCES "users"(id);

ALTER TABLE "import_profiles" ADD FOREIGN KEY ("user_id") REFERENCES "users"(id);

ALTER TABLE "manual_assets" ADD FOREIGN KEY ("user_id") REFERENCES "users"(id);

ALTER TABLE "plaid_items" ADD FOREIGN KEY ("institution_id") REFERENCES "institutions"(id);

ALTER TABLE "plaid_items" ADD FOREIGN KEY ("user_id") REFERENCES "users"(id);

ALTER TABLE "recurring_dismissals" ADD FOREIGN KEY ("user_id") REFERENCES "users"(id);

ALTER TABLE "recurring_items" ADD FOREIGN KEY ("account_id") REFERENCES "accounts"(id) ON DELETE SET NULL;

ALTER TABLE "recurring_items" ADD FOREIGN KEY ("user_id") REFERENCES "users"(id);

ALTER TABLE "transaction_rules" ADD FOREIGN KEY ("account_id") REFERENCES "accounts"(id) ON DELETE CASCADE;

ALTER TABLE "transaction_rules" ADD FOREIGN KEY ("category_id") REFERENCES "categories"(id);

ALTER TABLE "transaction_rules" ADD FOREIGN KEY ("user_id") REFERENCES "users"(id);

ALTER TABLE "transactions" ADD FOREIGN KEY ("account_id") REFERENCES "accounts"(id);

ALTER TABLE "transactions" ADD FOREIGN KEY ("category_id") REFERENCES "categories"(id);

ALTER TABLE "transactions" ADD FOREIGN KEY ("suggested_category_id") REFERENCES "categories"(id) ON DELETE SET NULL;

ALTER TABLE "transactions" ADD FOREIGN KEY ("plaid_item_id") REFERENCES "plaid_items"(id);

ALTER TABLE "transactions" ADD FOREIGN KEY ("matched_transaction_id") REFERENCES "transactions"(id) ON DELETE SET NULL;

ALTER TABLE "transactions" ADD FOREIGN KEY ("parent_id") REFERENCES "transactions"(id);

ALTER TABLE "transactions" ADD FOREIGN KEY ("user_id") REFERENCES "users"(id);
