-- Down: drop expenses.expenses table
DROP TABLE IF EXISTS expenses.expenses CASCADE;
DROP FUNCTION IF EXISTS expenses.expenses_audit_timestamp() CASCADE;
