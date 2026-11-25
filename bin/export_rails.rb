# Invoked with `bin/rails runner` by bin/import-rails. Output goes directly to Rust.
# No file containing decrypted tokens is written.
require "json"
Rails.logger = Logger.new(File::NULL)
ActiveRecord::Base.logger = nil
connection = ActiveRecord::Base.connection
schema = JSON.parse(File.read(File.expand_path("../src/schema.json", __dir__)))
result = { "version" => 1, "tables" => {} }
ActiveRecord::Base.transaction(isolation: :repeatable_read) do
  connection.execute("SET TRANSACTION READ ONLY")
  schema.keys.each do |table|
    # Read numeric PostgreSQL columns as text so JSON never rounds ledger values.
    columns = connection.columns(table)
    projection = columns.map do |column|
      identifier = connection.quote_column_name(column.name)
      column.type == :decimal ? "#{identifier}::text AS #{identifier}" : identifier
    end.join(", ")
    rows = connection.select_all("SELECT #{projection} FROM #{connection.quote_table_name(table)} ORDER BY id").to_a
    if table == "plaid_items"
      tokens = PlaidItem.all.to_h { |item| [item.id, item.access_token] }
      rows.each { |row| row["access_token"] = tokens.fetch(row["id"]); row["sync_requested_at"] = nil; row["sync_request_token"] = nil }
    end
    # pg's raw result leaves JSONB as strings; convert to their original objects.
    json_columns = columns.select { |c| [:json, :jsonb].include?(c.type) }.map(&:name)
    rows.each { |row| json_columns.each { |name| row[name] = JSON.parse(row[name]) if row[name].is_a?(String) } }
    result["tables"][table] = rows
  end
  result["verification"] = User.order(:id).map do |user|
    rows = user.transactions.reportable.pluck(:date, :iso_currency_code, :amount)
    months = rows.group_by { |date, currency, _| [date.strftime("%Y-%m"), currency.to_s] }.map do |(month, currency), entries|
      { "month" => month, "currency" => currency, "count" => entries.size,
        "spending" => entries.sum { |_, _, amount| amount.positive? ? amount : 0.to_d }.to_s("F"),
        "income" => entries.sum { |_, _, amount| amount.negative? ? -amount : 0.to_d }.to_s("F") }
    end
    { "user_id" => user.id, "months" => months }
  end
end
if ENV["WORTH_COPY_PLAID_CONFIG"] == "1"
  result["plaid_config"] = {
    "PLAID_CLIENT_ID" => ENV["PLAID_CLIENT_ID"].presence || Rails.application.credentials.dig(:plaid, :client_id),
    "PLAID_SECRET" => ENV["PLAID_SECRET"].presence || Rails.application.credentials.dig(:plaid, :secret),
    "PLAID_ENV" => ENV["PLAID_ENV"].presence || Rails.application.credentials.dig(:plaid, :env) || "sandbox"
  }.compact
end
STDOUT.write(JSON.generate(result))
