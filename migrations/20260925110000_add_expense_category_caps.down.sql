ALTER TABLE expenses.expense_categories
    DROP COLUMN IF EXISTS cap_per_claim,
    DROP COLUMN IF EXISTS cap_per_month;
