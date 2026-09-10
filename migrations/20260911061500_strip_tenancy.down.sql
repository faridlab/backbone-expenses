-- Hand-authored (user-owned). Not regenerated.
--
-- Best-effort restore sketch for the tenancy strip (ADR-0029). This is a breaking module
-- release against dev-stage databases: the down re-adds the company_id column as nullable
-- with the company-leading indexes in their original shapes, but restores NO data —
-- rows written after the strip (or after the decorator re-keyed them) carry org_unit_id
-- only. The composing service's tenancy decorator remains the live fence; treat this
-- down as a schema-shape sketch for archaeology, not a usable rollback.

ALTER TABLE expenses.expenses            ADD COLUMN IF NOT EXISTS company_id uuid;
ALTER TABLE expenses.expense_categories  ADD COLUMN IF NOT EXISTS company_id uuid;
ALTER TABLE expenses.expense_tax_lines   ADD COLUMN IF NOT EXISTS company_id uuid;

-- The strip's restored tenant-free domain indexes go away again (the company-leading
-- variants would need company data this sketch does not restore).
DROP INDEX IF EXISTS expenses.idx_expenses_employee_id_expense_date;
DROP INDEX IF EXISTS expenses.idx_expenses_state;

CREATE INDEX IF NOT EXISTS idx_expenses_company_id_employee_id_expense_date
    ON expenses.expenses (company_id, employee_id, expense_date);
CREATE INDEX IF NOT EXISTS idx_expenses_company_id_state
    ON expenses.expenses (company_id, state);
CREATE UNIQUE INDEX IF NOT EXISTS idx_expense_categories_company_id_code
    ON expenses.expense_categories (company_id, code) WHERE (metadata->>'deleted_at') IS NULL;
