# Read-only comparison data is piped to Rust; nothing is uploaded or saved.
require "json"
Rails.logger = Logger.new(File::NULL)
ActiveRecord::Base.logger = nil
result = []
ActiveRecord::Base.transaction(isolation: :repeatable_read) do
  ActiveRecord::Base.connection.execute("SET TRANSACTION READ ONLY")
  User.order(:id).each do |user|
    %w[USD CAD EUR GBP].each do |currency|
      report = FinancialReport.new(user: user, from: Date.new(1900, 1, 1), to: Date.current, currency: currency)
      history = FinancialReport.net_worth_history(user, currency: currency, from: Date.new(1900, 1, 1), to: Date.current)
      result << { user_id: user.id, currency: currency,
        spending: report.spending.to_s("F"), income: report.income.to_s("F"),
        history: history.map { |date, value, known, total, known_assets, total_assets| { date: date.iso8601, value: value.to_s("F"), known_accounts: known, total_accounts: total, known_assets: known_assets, total_assets: total_assets } } }
    end
  end
end
STDOUT.write(JSON.generate(result))
