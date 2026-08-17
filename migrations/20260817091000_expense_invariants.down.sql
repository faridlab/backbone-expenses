-- Reverse the expense invariants.

ALTER TABLE expenses.expenses
    DROP CONSTRAINT IF EXISTS expenses_state_pair_legal;

ALTER TABLE expenses.expenses
    DROP CONSTRAINT IF EXISTS expenses_amount_total_nonneg;

ALTER TABLE expenses.expense_tax_lines
    DROP CONSTRAINT IF EXISTS expense_tax_lines_tax_amount_nonneg;
