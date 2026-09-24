-- Migration: per-claim and per-month caps on expense_categories
--
-- A category can now say what is too much: one claim over `cap_per_claim`
-- refuses at submit, and one employee's live claims for the calendar month
-- over `cap_per_month` refuse the claim that would cross the line. NULL
-- keeps today's uncapped behaviour.

ALTER TABLE expenses.expense_categories
    ADD COLUMN IF NOT EXISTS cap_per_claim NUMERIC(18, 2)
        CHECK (cap_per_claim IS NULL OR cap_per_claim > 0),
    ADD COLUMN IF NOT EXISTS cap_per_month NUMERIC(18, 2)
        CHECK (cap_per_month IS NULL OR cap_per_month > 0);
