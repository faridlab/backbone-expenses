-- Hand-authored (user-owned). Not regenerated.
--
-- Strip every company-fence artifact from the expenses tables (ADR-0029): the module is
-- tenant-agnostic; org scoping is installed by the COMPOSING service's tenancy decorator,
-- never by the module. Dropped here, per table: the company-leading indexes, the
-- <table>_company_isolation RLS policy, and the company_id column itself.
--
-- Ordering guard (the decorator must run FIRST on any database with data): the module
-- never moves tenancy data. A table is safe to strip when EITHER
--   a) it carries org_unit_id with no NULLs — the decorator backfilled it from company_id —
--      or b) it is empty (a fresh database: the earlier chain files created it empty).
-- Otherwise the strip RAISEs, naming the decorator step, rather than dropping a column
-- that still holds the only tenancy key. The file is re-runnable (every drop is IF EXISTS
-- and the tracker has no checksums), so a failed run retries cleanly after the decorator
-- lands.
--
-- RLS enable/force flags are deliberately NOT touched: the decorator owns those now.

DO $$
DECLARE
    t text;
    has_org boolean;
    org_nulls bigint;
    total bigint;
    offenders text := '';
BEGIN
    FOREACH t IN ARRAY ARRAY['expenses', 'expense_categories', 'expense_tax_lines']
    LOOP
        IF to_regclass(format('expenses.%I', t)) IS NULL THEN
            CONTINUE; -- chain not fully applied on this database; nothing to strip
        END IF;

        SELECT EXISTS (
                   SELECT 1 FROM information_schema.columns
                   WHERE table_schema = 'expenses' AND table_name = t AND column_name = 'org_unit_id'
               )
        INTO has_org;

        EXECUTE format('SELECT count(*) FROM expenses.%I', t) INTO total;

        IF has_org THEN
            EXECUTE format(
                'SELECT count(*) FROM expenses.%I WHERE org_unit_id IS NULL', t)
            INTO org_nulls;
        ELSE
            org_nulls := total; -- no org column: every row's only tenancy key is company_id
        END IF;

        IF has_org AND org_nulls = 0 THEN
            CONTINUE; -- decorator backfilled: safe
        END IF;
        IF total = 0 THEN
            CONTINUE; -- empty table (fresh database): safe
        END IF;
        offenders := offenders || format(' expenses.%s (%s rows, %s rows not covered by org_unit_id);', t, total, org_nulls);
    END LOOP;

    IF offenders <> '' THEN
        RAISE EXCEPTION 'refusing to strip company_id — these tables are not yet covered by the tenancy decorator:%. Apply the composing service''s tenancy decorator (it backfills org_unit_id from company_id) and re-run; it is the only step that moves tenancy data.', offenders;
    END IF;
END $$;

-- ── expenses ───────────────────────────────────────────────────────────────────
DROP INDEX IF EXISTS expenses.idx_expenses_company_id_employee_id_expense_date;
DROP INDEX IF EXISTS expenses.idx_expenses_company_id_state;
DROP POLICY IF EXISTS expenses_company_isolation ON expenses.expenses;
ALTER TABLE expenses.expenses DROP COLUMN IF EXISTS company_id;

-- ── expense_categories ─────────────────────────────────────────────────────────
DROP INDEX IF EXISTS expenses.idx_expense_categories_company_id_code;
DROP POLICY IF EXISTS expense_categories_company_isolation ON expenses.expense_categories;
ALTER TABLE expenses.expense_categories DROP COLUMN IF EXISTS company_id;

-- ── expense_tax_lines ──────────────────────────────────────────────────────────
DROP POLICY IF EXISTS expense_tax_lines_company_isolation ON expenses.expense_tax_lines;
ALTER TABLE expenses.expense_tax_lines DROP COLUMN IF EXISTS company_id;

-- ── Restore the tenant-free domain indexes ─────────────────────────────────────
-- The report projection and the workflow-queue scan are DOMAIN reads, not tenancy
-- posture: grouping claims by employee over a date range and draining the
-- to-approve / to-post / to-settle queues need no tenant column. These re-base the
-- dropped company-leading indexes onto their tenant-free column sets. The per-unit
-- category-code unique is POSTURE and is owned by the composing service's tenancy
-- decorator — it is intentionally NOT restored here (the pre-strip global-per-company
-- form is not this module's concern).
CREATE INDEX IF NOT EXISTS idx_expenses_employee_id_expense_date
    ON expenses.expenses (employee_id, expense_date);
CREATE INDEX IF NOT EXISTS idx_expenses_state
    ON expenses.expenses (state);
