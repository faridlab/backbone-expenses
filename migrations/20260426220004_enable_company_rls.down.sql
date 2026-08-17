-- Down: remove the company RLS fence for __module__ module

-- Reverse the company RLS fence for expenses.expenses
DROP POLICY IF EXISTS expenses_company_isolation ON expenses.expenses;
ALTER TABLE expenses.expenses NO FORCE ROW LEVEL SECURITY;
ALTER TABLE expenses.expenses DISABLE ROW LEVEL SECURITY;

-- Reverse the company RLS fence for expenses.expense_categories
DROP POLICY IF EXISTS expense_categories_company_isolation ON expenses.expense_categories;
ALTER TABLE expenses.expense_categories NO FORCE ROW LEVEL SECURITY;
ALTER TABLE expenses.expense_categories DISABLE ROW LEVEL SECURITY;

-- Reverse the company RLS fence for expenses.expense_tax_lines
DROP POLICY IF EXISTS expense_tax_lines_company_isolation ON expenses.expense_tax_lines;
ALTER TABLE expenses.expense_tax_lines NO FORCE ROW LEVEL SECURITY;
ALTER TABLE expenses.expense_tax_lines DISABLE ROW LEVEL SECURITY;

