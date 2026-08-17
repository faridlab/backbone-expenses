-- Down: drop expenses.expense_tax_lines table
DROP TABLE IF EXISTS expenses.expense_tax_lines CASCADE;
DROP FUNCTION IF EXISTS expenses.expense_tax_lines_audit_timestamp() CASCADE;
