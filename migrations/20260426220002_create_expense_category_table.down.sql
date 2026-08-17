-- Down: drop expenses.expense_categories table
DROP TABLE IF EXISTS expenses.expense_categories CASCADE;
DROP FUNCTION IF EXISTS expenses.expense_categories_audit_timestamp() CASCADE;
