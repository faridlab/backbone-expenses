-- Down: drop enum types for __module__ module
DROP TYPE IF EXISTS expense_state CASCADE;
DROP TYPE IF EXISTS expense_approval_state CASCADE;
DROP TYPE IF EXISTS expense_payment_mode CASCADE;
