-- DB-level invariants for expenses (Wave 1 P3, pillar-people H-4). The
-- two-field lifecycle (ADR-0016): `approval_state` is hand-set by the
-- submit/approve/refuse verbs, `state` is the financial lifecycle. The DB is
-- the arbiter of which (approval_state, state) pairs are legal (the W1 P2
-- doctrine), so no code path — write service, generic CRUD, or direct SQL —
-- can land an inconsistent pair. HEM-C1 amounts are non-negative at the DB:
-- an expense is a claim, not a correction instrument (refunds are new rows).

ALTER TABLE expenses.expenses
    ADD CONSTRAINT expenses_amount_total_nonneg
    CHECK (amount_total >= 0);

ALTER TABLE expenses.expenses
    ADD CONSTRAINT expenses_state_pair_legal
    CHECK (
        (approval_state = 'draft'     AND state = 'draft') OR
        (approval_state = 'submitted' AND state = 'submitted') OR
        (approval_state = 'approved'  AND state IN ('approved', 'posted', 'done')) OR
        (approval_state = 'refused'   AND state = 'refused')
    );

ALTER TABLE expenses.expense_tax_lines
    ADD CONSTRAINT expense_tax_lines_tax_amount_nonneg
    CHECK (tax_amount >= 0);
