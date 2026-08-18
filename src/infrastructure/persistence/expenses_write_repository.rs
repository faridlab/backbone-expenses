//! `ExpensesWriteRepository` — every SQL statement the validated write path issues
//! (hand-authored, user-owned; see `metaphor.codegen.yaml`).
//!
//! Mirrors the attendance/timeoff family convention: the service owns the verbs and the
//! transactions; this repo owns the statements. All reads/writes ride the caller's bound
//! connection — `company_scope::bind_company_on` was applied by the service — so the ADR-0008
//! RLS fence scopes every statement (`WHERE company_id = $x` is kept anyway as belt-and-braces;
//! a cross-company id simply matches zero rows, surfacing as 404, never as leakage).
//!
//! Soft-delete lives in `metadata` JSONB (`deleted_at` key), so every "live row" predicate is
//! `(metadata->>'deleted_at') IS NULL`, mirroring the module's partial indexes and the fence.
//! The `(approval_state, state)` table CHECK (`expenses_state_pair_legal`) is the arbiter of
//! legal pairs — the service only ever emits legal ones; a state-guard UPDATE matching zero rows
//! surfaces as a 409 race error.

use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::Decimal;
use uuid::Uuid;

use backbone_orm::company_scope;

use crate::domain::entity::{Expense, ExpenseCategory, ExpenseTaxLine};

/// One grouped row of the report projection (read-only SQL — no entity, HEM-13).
#[derive(Debug, serde::Serialize, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct ExpenseReportRow {
    pub employee_id: Uuid,
    pub category_id: Uuid,
    pub category_code: String,
    pub category_name: String,
    pub state: String,
    pub currency: String,
    pub line_count: i64,
    pub amount_total: Decimal,
}

/// A tax-line row to write via [`ExpensesWriteRepository::replace_tax_lines`]. Pre-computed by
/// the caller (billing's removable-overlay pattern — expenses does no tax computation).
#[derive(Debug, Clone)]
pub struct TaxLineWrite {
    pub basis: String,
    pub account_id: Uuid,
    pub description: Option<String>,
    pub rate: Decimal,
    pub tax_amount: Decimal,
}

#[derive(Debug, Default, Clone)]
pub struct ExpensePatch {
    pub category_id: Option<Uuid>,
    pub expense_date: Option<NaiveDate>,
    pub description: Option<String>,
    pub amount_total: Option<Decimal>,
    pub currency: Option<String>,
    pub payment_mode: Option<String>,
    pub reference: Option<Option<String>>,
}

pub struct ExpensesWriteRepository;

impl ExpensesWriteRepository {
    // ── categories ──────────────────────────────────────────────────────────

    /// The live category for this company (create/post validation). A cross-company category
    /// id matches zero rows → 404, never leakage.
    pub async fn find_category(
        &self,
        conn: &mut sqlx::PgConnection,
        company: Uuid,
        category_id: Uuid,
    ) -> Result<Option<ExpenseCategory>, sqlx::Error> {
        sqlx::query_as::<_, ExpenseCategory>(
            r#"SELECT * FROM expenses.expense_categories
                WHERE company_id = $1 AND id = $2
                  AND (metadata->>'deleted_at') IS NULL"#,
        )
        .bind(company)
        .bind(category_id)
        .fetch_optional(&mut *conn)
        .await
    }

    // ── expenses: reads ─────────────────────────────────────────────────────

    /// One live expense row (row-truth for every verb guard).
    pub async fn get_expense(
        &self,
        conn: &mut sqlx::PgConnection,
        company: Uuid,
        expense_id: Uuid,
    ) -> Result<Option<Expense>, sqlx::Error> {
        sqlx::query_as::<_, Expense>(
            r#"SELECT * FROM expenses.expenses
                WHERE company_id = $1 AND id = $2
                  AND (metadata->>'deleted_at') IS NULL"#,
        )
        .bind(company)
        .bind(expense_id)
        .fetch_optional(&mut *conn)
        .await
    }

    // ── expenses: writes ────────────────────────────────────────────────────

    /// Insert a new draft claim; `created_by` is stamped into the audit metadata.
    pub async fn insert_expense(
        &self,
        conn: &mut sqlx::PgConnection,
        expense: &Expense,
        actor: Option<Uuid>,
        now: DateTime<Utc>,
    ) -> Result<Expense, sqlx::Error> {
        sqlx::query_as::<_, Expense>(
            r#"INSERT INTO expenses.expenses
                   (id, company_id, employee_id, category_id, expense_date, description,
                    amount_total, currency, payment_mode, reference, approval_state, state,
                    receipt_file_id, metadata)
               VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9::expense_payment_mode, $10,
                       'draft', 'draft', $11,
                       jsonb_build_object('created_by', to_jsonb($12::uuid), 'created_at', to_jsonb($13::timestamptz)))
               RETURNING *"#,
        )
        .bind(expense.id)
        .bind(expense.company_id)
        .bind(expense.employee_id)
        .bind(expense.category_id)
        .bind(expense.expense_date)
        .bind(&expense.description)
        .bind(expense.amount_total)
        .bind(&expense.currency)
        .bind(expense.payment_mode.to_string())
        .bind(&expense.reference)
        .bind(expense.receipt_file_id)
        .bind(actor)
        .bind(now)
        .fetch_one(&mut *conn)
        .await
    }

    /// Draft-only field update. The ROW-TRUTH guard (P2 doctrine): the WHERE matches only a
    /// row that is still draft/draft — a concurrently submitted/refused expense matches zero
    /// rows and the service reports a 409, regardless of what the payload claims.
    pub async fn update_expense(
        &self,
        conn: &mut sqlx::PgConnection,
        company: Uuid,
        expense_id: Uuid,
        patch: &ExpensePatch,
        actor: Option<Uuid>,
        now: DateTime<Utc>,
    ) -> Result<Option<Expense>, sqlx::Error> {
        sqlx::query_as::<_, Expense>(
            r#"UPDATE expenses.expenses SET
                   category_id    = COALESCE($3, category_id),
                   expense_date   = COALESCE($4, expense_date),
                   description    = COALESCE($5, description),
                   amount_total   = COALESCE($6, amount_total),
                   currency       = COALESCE($7, currency),
                   payment_mode   = COALESCE($8::expense_payment_mode, payment_mode),
                   reference      = CASE WHEN $9 THEN $10 ELSE reference END,
                   metadata       = metadata || jsonb_build_object(
                                       'updated_by', to_jsonb($11::uuid),
                                       'updated_at', to_jsonb($12::timestamptz))
               WHERE company_id = $1 AND id = $2
                 AND approval_state = 'draft' AND state = 'draft'
                 AND (metadata->>'deleted_at') IS NULL
               RETURNING *"#,
        )
        .bind(company)
        .bind(expense_id)
        .bind(patch.category_id)
        .bind(patch.expense_date)
        .bind(&patch.description)
        .bind(patch.amount_total)
        .bind(&patch.currency)
        .bind(patch.payment_mode.as_deref())
        .bind(patch.reference.is_some())
        .bind(patch.reference.clone().flatten())
        .bind(actor)
        .bind(now)
        .fetch_optional(&mut *conn)
        .await
    }

    /// draft → submitted, stamping the approval link (None when the seam is unwired).
    /// The guard matches not just the state pair but the
    /// exact payload that was filed with the approvals engine: a concurrent draft edit
    /// between the service's read and this write changes one of the filed fields, the
    /// guard matches zero rows, and the caller surfaces the 409 — the row is never linked
    /// to a filing whose snapshot no longer describes it. The client retries against the
    /// fresh row and the engine's idempotent per-resource filing returns the same request.
    pub async fn mark_submitted(
        &self,
        conn: &mut sqlx::PgConnection,
        company: Uuid,
        expense_id: Uuid,
        approval_request_id: Option<Uuid>,
        actor: Option<Uuid>,
        now: DateTime<Utc>,
        filed_employee: Uuid,
        filed_category: Uuid,
        filed_date: NaiveDate,
        filed_amount: Decimal,
        filed_currency: &str,
    ) -> Result<Option<Expense>, sqlx::Error> {
        sqlx::query_as::<_, Expense>(
            r#"UPDATE expenses.expenses SET
                   approval_state = 'submitted',
                   state = 'submitted',
                   approval_request_id = $3,
                   metadata = metadata || jsonb_build_object(
                       'submitted_by', to_jsonb($4::uuid),
                       'updated_at', to_jsonb($5::timestamptz))
               WHERE company_id = $1 AND id = $2
                 AND approval_state = 'draft' AND state = 'draft'
                 AND employee_id = $6 AND category_id = $7
                 AND expense_date = $8 AND amount_total = $9 AND currency = $10
                 AND (metadata->>'deleted_at') IS NULL
               RETURNING *"#,
        )
        .bind(company)
        .bind(expense_id)
        .bind(approval_request_id)
        .bind(actor)
        .bind(now)
        .bind(filed_employee)
        .bind(filed_category)
        .bind(filed_date)
        .bind(filed_amount)
        .bind(filed_currency)
        .fetch_optional(&mut *conn)
        .await
    }

    /// submitted → approved (verdict already honored by the service — TR2 fail-closed upstream).
    pub async fn mark_approved(
        &self,
        conn: &mut sqlx::PgConnection,
        company: Uuid,
        expense_id: Uuid,
        actor: Option<Uuid>,
        now: DateTime<Utc>,
    ) -> Result<Option<Expense>, sqlx::Error> {
        sqlx::query_as::<_, Expense>(
            r#"UPDATE expenses.expenses SET
                   approval_state = 'approved',
                   state = 'approved',
                   metadata = metadata || jsonb_build_object(
                       'approved_by', to_jsonb($3::uuid),
                       'updated_at', to_jsonb($4::timestamptz))
               WHERE company_id = $1 AND id = $2
                 AND approval_state = 'submitted' AND state = 'submitted'
                 AND (metadata->>'deleted_at') IS NULL
               RETURNING *"#,
        )
        .bind(company)
        .bind(expense_id)
        .bind(actor)
        .bind(now)
        .fetch_optional(&mut *conn)
        .await
    }

    /// submitted → refused (sticky). The reason is kept in the audit metadata for the report.
    pub async fn mark_refused(
        &self,
        conn: &mut sqlx::PgConnection,
        company: Uuid,
        expense_id: Uuid,
        reason: Option<&str>,
        actor: Option<Uuid>,
        now: DateTime<Utc>,
    ) -> Result<Option<Expense>, sqlx::Error> {
        sqlx::query_as::<_, Expense>(
            r#"UPDATE expenses.expenses SET
                   approval_state = 'refused',
                   state = 'refused',
                   metadata = metadata || jsonb_build_object(
                       'refused_by', to_jsonb($3::uuid),
                       'refused_reason', to_jsonb($4::text),
                       'updated_at', to_jsonb($5::timestamptz))
               WHERE company_id = $1 AND id = $2
                 AND approval_state = 'submitted' AND state = 'submitted'
                 AND (metadata->>'deleted_at') IS NULL
               RETURNING *"#,
        )
        .bind(company)
        .bind(expense_id)
        .bind(actor)
        .bind(reason)
        .bind(now)
        .fetch_optional(&mut *conn)
        .await
    }

    /// approved → posted, stamping the GL ack (`journal_id` + `accounting_post_id`). The state
    /// guard makes a double post match zero rows → the service reports 409; the idempotency key
    /// makes even a raced envelope reuse accounting's dedup.
    pub async fn mark_posted(
        &self,
        conn: &mut sqlx::PgConnection,
        company: Uuid,
        expense_id: Uuid,
        journal_id: Uuid,
        accounting_post_id: Uuid,
        actor: Option<Uuid>,
        now: DateTime<Utc>,
    ) -> Result<Option<Expense>, sqlx::Error> {
        sqlx::query_as::<_, Expense>(
            r#"UPDATE expenses.expenses SET
                   state = 'posted',
                   journal_id = $3,
                   accounting_post_id = $4,
                   metadata = metadata || jsonb_build_object(
                       'posted_by', to_jsonb($5::uuid),
                       'updated_at', to_jsonb($6::timestamptz))
               WHERE company_id = $1 AND id = $2
                 AND approval_state = 'approved' AND state = 'approved'
                 AND (metadata->>'deleted_at') IS NULL
               RETURNING *"#,
        )
        .bind(company)
        .bind(expense_id)
        .bind(journal_id)
        .bind(accounting_post_id)
        .bind(actor)
        .bind(now)
        .fetch_optional(&mut *conn)
        .await
    }

    /// posted → done (own_account only — the service guards payment_mode), stamping the
    /// reimbursement ack id.
    pub async fn mark_settled(
        &self,
        conn: &mut sqlx::PgConnection,
        company: Uuid,
        expense_id: Uuid,
        reimbursement_id: Uuid,
        actor: Option<Uuid>,
        now: DateTime<Utc>,
    ) -> Result<Option<Expense>, sqlx::Error> {
        sqlx::query_as::<_, Expense>(
            r#"UPDATE expenses.expenses SET
                   state = 'done',
                   reimbursement_id = $3,
                   metadata = metadata || jsonb_build_object(
                       'settled_by', to_jsonb($4::uuid),
                       'updated_at', to_jsonb($5::timestamptz))
               WHERE company_id = $1 AND id = $2
                 AND state = 'posted'
                 AND (metadata->>'deleted_at') IS NULL
               RETURNING *"#,
        )
        .bind(company)
        .bind(expense_id)
        .bind(reimbursement_id)
        .bind(actor)
        .bind(now)
        .fetch_optional(&mut *conn)
        .await
    }

    /// Attach/detach the receipt scan. Allowed while the claim is still open
    /// (draft or submitted) — once decided, the evidence set is fixed.
    pub async fn set_receipt(
        &self,
        conn: &mut sqlx::PgConnection,
        company: Uuid,
        expense_id: Uuid,
        receipt_file_id: Option<Uuid>,
        actor: Option<Uuid>,
        now: DateTime<Utc>,
    ) -> Result<Option<Expense>, sqlx::Error> {
        sqlx::query_as::<_, Expense>(
            r#"UPDATE expenses.expenses SET
                   receipt_file_id = $3,
                   metadata = metadata || jsonb_build_object(
                       'updated_by', to_jsonb($4::uuid),
                       'updated_at', to_jsonb($5::timestamptz))
               WHERE company_id = $1 AND id = $2
                 AND approval_state IN ('draft', 'submitted')
                 AND (metadata->>'deleted_at') IS NULL
               RETURNING *"#,
        )
        .bind(company)
        .bind(expense_id)
        .bind(receipt_file_id)
        .bind(actor)
        .bind(now)
        .fetch_optional(&mut *conn)
        .await
    }

    // ── tax overlay ─────────────────────────────────────────────────────────

    /// The live overlay rows for one expense (envelope building).
    pub async fn tax_lines_for(
        &self,
        conn: &mut sqlx::PgConnection,
        company: Uuid,
        expense_id: Uuid,
    ) -> Result<Vec<ExpenseTaxLine>, sqlx::Error> {
        sqlx::query_as::<_, ExpenseTaxLine>(
            r#"SELECT * FROM expenses.expense_tax_lines
                WHERE company_id = $1 AND expense_id = $2
                  AND (metadata->>'deleted_at') IS NULL
                ORDER BY id"#,
        )
        .bind(company)
        .bind(expense_id)
        .fetch_all(&mut *conn)
        .await
    }

    /// Replace the overlay atomically (delete + re-insert in the caller's tx). Draft-only is
    /// enforced BY THIS SQL — every statement is row-truth-guarded on the parent expense's
    /// `(draft, draft)` pair (the same compare-and-set shape as `update_expense`), so a
    /// submit that commits mid-replace makes the guard match zero rows and the service
    /// reports 409. The council F1 fix: the service-level pre-read alone left a race window
    /// where a raced overlay write inflated the gross on a submitted claim.
    ///
    /// Returns the number of lines actually inserted — the service requires it to equal
    /// `lines.len()` or the whole transaction rolls back as `NotDraft`.
    pub async fn replace_tax_lines(
        &self,
        conn: &mut sqlx::PgConnection,
        company: Uuid,
        expense_id: Uuid,
        lines: &[TaxLineWrite],
        actor: Option<Uuid>,
        now: DateTime<Utc>,
    ) -> Result<u64, sqlx::Error> {
        sqlx::query(
            r#"UPDATE expenses.expense_tax_lines SET
                   metadata = metadata || jsonb_build_object('deleted_at', to_jsonb($3::timestamptz))
               WHERE company_id = $1 AND expense_id = $2
                 AND (metadata->>'deleted_at') IS NULL
                 AND EXISTS (SELECT 1 FROM expenses.expenses e
                              WHERE e.company_id = $1 AND e.id = $2
                                AND e.approval_state = 'draft' AND e.state = 'draft'
                                AND (e.metadata->>'deleted_at') IS NULL)"#,
        )
        .bind(company)
        .bind(expense_id)
        .bind(now)
        .execute(&mut *conn)
        .await?;

        let mut inserted = 0u64;
        for line in lines {
            let n = sqlx::query(
                r#"INSERT INTO expenses.expense_tax_lines
                       (id, company_id, expense_id, basis, account_id, description, rate, tax_amount, metadata)
                   SELECT $1, $2, $3, $4, $5, $6, $7, $8,
                          jsonb_build_object('created_by', to_jsonb($9::uuid), 'created_at', to_jsonb($10::timestamptz))
                   WHERE EXISTS (SELECT 1 FROM expenses.expenses e
                                  WHERE e.company_id = $2 AND e.id = $3
                                    AND e.approval_state = 'draft' AND e.state = 'draft'
                                    AND (e.metadata->>'deleted_at') IS NULL)"#,
            )
            .bind(Uuid::new_v4())
            .bind(company)
            .bind(expense_id)
            .bind(&line.basis)
            .bind(line.account_id)
            .bind(&line.description)
            .bind(line.rate)
            .bind(line.tax_amount)
            .bind(actor)
            .bind(now)
            .execute(&mut *conn)
            .await?
            .rows_affected();
            inserted += n;
        }
        Ok(inserted)
    }

    // ── report projection (read-only, HEM-13 — grouping is NOT an entity) ────

    /// Grouped totals per employee × category × state over a date range. Read-only SQL on the
    /// request-scoped connection (`fetch_all_scoped`): RLS-fenced, no write surface at all.
    pub async fn report(
        &self,
        pool: &sqlx::PgPool,
        company: Uuid,
        employee_id: Option<Uuid>,
        from: NaiveDate,
        to: NaiveDate,
    ) -> Result<Vec<ExpenseReportRow>, sqlx::Error> {
        let q = sqlx::query_as::<_, ExpenseReportRow>(
            r#"SELECT e.employee_id,
                      e.category_id,
                      c.code   AS category_code,
                      c.name   AS category_name,
                      e.state::text AS state,
                      e.currency,
                      COUNT(*)     AS line_count,
                      SUM(e.amount_total) AS amount_total
               FROM expenses.expenses e
               JOIN expenses.expense_categories c ON c.id = e.category_id AND c.company_id = e.company_id
               WHERE e.company_id = $1
                 AND e.expense_date >= $2
                 AND e.expense_date <= $3
                 AND ($4::uuid IS NULL OR e.employee_id = $4)
                 AND (e.metadata->>'deleted_at') IS NULL
               GROUP BY e.employee_id, e.category_id, c.code, c.name, e.state, e.currency
               ORDER BY e.employee_id, c.code, e.state"#,
        )
        .bind(company)
        .bind(from)
        .bind(to)
        .bind(employee_id);
        company_scope::fetch_all_scoped(pool, q).await
    }
}
