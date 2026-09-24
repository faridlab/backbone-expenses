//! `ExpensesWriteService` — the validated expense write path (H-4, Wave 1 P3).
//!
//! Hand-written (user-owned — see `metaphor.codegen.yaml`). Mirrors the family shape
//! (party v0.3.3 / timeoff P1 / attendance P2): a concrete struct, an error enum carrying
//! `code()`/`http_status()`, transaction-per-operation with ambient-scope relay, and all SQL
//! delegated to [`crate::infrastructure::persistence::ExpensesWriteRepository`].
//!
//! Tenancy: none, by design (ADR-0029). The module is tenant-agnostic — no tenant key on any
//! write, no scope parameter on any verb. Every transaction relays the AMBIENT request org
//! scope, when the composing service bound one (`backbone_orm::org_scope::bind_org_scope_on`):
//! the decorator-installed row-level fences govern which rows a verb can see and write. A
//! deployment that runs unfenced gets an unfenced module. The three outbound seams still key
//! on a company (approvals / GL / reimbursement are unstripped consumers) and source the
//! documented legacy twin from the ambient scope, failing closed — the module never guesses.
//!
//! Lifecycle doctrine (ADR-0016, the TWO-FIELD SPLIT): `approval_state` is hand-set by these
//! verbs; `state` is the financial lifecycle that follows approval and then advances
//! approved → posted (GL ack) → done (reimbursement ack). Legal `(approval_state, state)` pairs
//! are enforced by the `expenses_state_pair_legal` table CHECK — the DB is the arbiter (P2
//! doctrine); the row-truth state guards below (`WHERE approval_state = '…'`) make every verb a
//! compare-and-set, so a raced verb matches zero rows and surfaces as 409, never a corrupt pair.
//!
//! Seams (ADR-0004 — zero Cargo edges between shipped modules):
//! - **Approvals** ([`ApprovalFiling`]): submit files FIRST, outside the tx (a network call must
//!   not hold a row lock); unwired seam ⇒ no link, exactly as timeoff P1. `approve` is TR2
//!   FAIL-CLOSED: a claim linked into the engine is granted ONLY by an `Approved` verdict —
//!   unwired/unknown ⇒ 409, never a bypass.
//! - **GL** ([`GlPostSink`]): `post` builds ONE envelope per expense (HEM-13, no sheet), asserts
//!   it balances, and calls the sink outside the tx. Unwired ⇒ `gl_seam_unwired` (422); the row
//!   stays `approved` and retryable. Accounting lands in W2.
//! - **Reimbursement** ([`ReimbursementSink`]): `settle` (posted + own_account only) calls the
//!   sink outside the tx; the ack id stamps `reimbursement_id` and the row reaches `done`.
//!   Unwired ⇒ fail closed. Payment composes in W2.

use chrono::{DateTime, Datelike, NaiveDate, Utc};
use rust_decimal::Decimal;
use sqlx::PgPool;
use std::sync::{Arc, RwLock};
use uuid::Uuid;

use backbone_orm::org_scope;

use crate::domain::entity::{Expense, ExpensePaymentMode};
use crate::infrastructure::persistence::{
    ExpensePatch, ExpenseReportRow, ExpensesWriteRepository, TaxLineWrite,
};

use super::approvals_port::{
    ApprovalFiling, ApprovalSeamError, ApprovalVerdict, ExpenseApprovalFilingRequest,
    UnwiredApprovals,
};
use super::expense_gl::{build_expense_envelope, PostAccounts, TaxLineInput};
use super::reimbursement_port::{ReimbursementRequest, ReimbursementSeamError, ReimbursementSink, UnwiredReimbursement};
use super::GlPostSink;

// ─── error surface ────────────────────────────────────────────────────────────

/// Errors the write path can produce. `code()` is the stable machine string, `http_status()` the
/// mapped status — both consumed by the guarded routes' `err_response`.
#[derive(Debug, thiserror::Error)]
pub enum ExpenseWriteError {
    #[error("expense not found")]
    NotFound,
    #[error("expense category not found")]
    CategoryNotFound,
    #[error("amount must be zero or greater — an expense is a claim, not a correction")]
    NegativeAmount,
    #[error("currency must be a 3-letter ISO code (single-currency v1: IDR)")]
    BadCurrency,
    #[error("category cap {kind} exceeded: cap {cap}, claim would total {amount}")]
    CategoryCapExceeded {
        kind: &'static str,
        cap: rust_decimal::Decimal,
        amount: rust_decimal::Decimal,
    },
    #[error("payment mode must be \"own_account\" or \"company_account\"")]
    BadPaymentMode,
    #[error("tax line basis must be \"input\" or \"withholding\"")]
    BadTaxBasis,
    #[error("`from` must be a date before or equal to `to`")]
    BadDateRange,
    #[error("expense is not a draft — the row's own state decides, not the payload")]
    NotDraft,
    #[error("expense is not submitted")]
    NotSubmitted,
    #[error("expense is not approved for posting")]
    NotApprovedForPost,
    #[error("expense is already posted")]
    AlreadyPosted,
    #[error("expense is not posted")]
    NotPosted,
    #[error("approval not granted — a claim linked into the approvals engine is granted only by the engine (TR2)")]
    ApprovalNotGranted,
    #[error("company-account expenses settle at the bank when posted — there is nothing to reimburse")]
    NotReimbursable,
    #[error("GL seam rejected the post: {code}: {message}")]
    GlRejected { code: String, message: String },
    #[error("the reimbursement seam is not wired — supply a ReimbursementSink to settle expenses")]
    ReimbursementUnwired,
    #[error("reimbursement seam error: {0}")]
    ReimbursementTransport(String),
    #[error("approvals seam error: {0}")]
    ApprovalTransport(String),
    #[error("no org scope bound: the composing service must resolve one for this request")]
    NoCompanyScope,
    #[error("internal error: {0}")]
    Internal(String),
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

impl ExpenseWriteError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::NotFound => "expense_not_found",
            Self::CategoryNotFound => "expense_category_not_found",
            Self::NegativeAmount => "negative_amount",
            Self::BadCurrency => "bad_currency",
            Self::BadPaymentMode => "bad_payment_mode",
            Self::BadTaxBasis => "bad_tax_basis",
            Self::BadDateRange => "bad_date_range",
            Self::NotDraft => "not_draft",
            Self::NotSubmitted => "not_submitted",
            Self::NotApprovedForPost => "not_approved_for_post",
            Self::AlreadyPosted => "already_posted",
            Self::NotPosted => "not_posted",
            Self::ApprovalNotGranted => "approval_not_granted",
            Self::NotReimbursable => "not_reimbursable",
            Self::GlRejected { .. } => "gl_post_rejected",
            Self::CategoryCapExceeded { .. } => "category_cap_exceeded",
            Self::ReimbursementUnwired => "reimbursement_seam_unwired",
            Self::ReimbursementTransport(_) => "reimbursement_seam_error",
            Self::ApprovalTransport(_) => "approvals_seam_error",
            Self::NoCompanyScope => "no_org_scope",
            Self::Internal(_) => "internal_error",
            Self::Db(_) => "database_error",
        }
    }

    pub fn http_status(&self) -> u16 {
        match self {
            Self::NotFound | Self::CategoryNotFound => 404,
            Self::NotDraft | Self::NotSubmitted | Self::NotApprovedForPost
            | Self::AlreadyPosted | Self::NotPosted | Self::ApprovalNotGranted
            | Self::NotReimbursable => 409,
            Self::NegativeAmount | Self::BadCurrency | Self::BadPaymentMode | Self::BadTaxBasis
            | Self::BadDateRange
            | Self::CategoryCapExceeded { .. }
            | Self::GlRejected { .. } | Self::ReimbursementUnwired
            | Self::ReimbursementTransport(_) | Self::ApprovalTransport(_) => 422,
            Self::NoCompanyScope | Self::Internal(_) | Self::Db(_) => 500,
        }
    }
}

impl From<ApprovalSeamError> for ExpenseWriteError {
    fn from(e: ApprovalSeamError) -> Self {
        match e {
            ApprovalSeamError::Transport(m) => Self::ApprovalTransport(m),
            // Unwired/Unknown are handled BY THE VERBS (Unwired ⇒ no link at submit;
            // Unknown ⇒ fail-closed at approve) — reaching here means a coding slip upstream.
            other => Self::Internal(format!("approvals seam: {other}")),
        }
    }
}

impl From<ReimbursementSeamError> for ExpenseWriteError {
    fn from(e: ReimbursementSeamError) -> Self {
        match e {
            ReimbursementSeamError::Unwired => Self::ReimbursementUnwired,
            ReimbursementSeamError::Transport(m) => Self::ReimbursementTransport(m),
        }
    }
}

// ─── inputs / outcomes ────────────────────────────────────────────────────────

/// What `create_expense` accepts. Amount ≥ 0, ISO-3 currency, a live category —
/// validated here, backstopped by the DB CHECKs.
#[derive(Debug)]
pub struct NewExpense {
    pub employee_id: Uuid,
    pub category_id: Uuid,
    pub expense_date: NaiveDate,
    pub description: String,
    pub amount_total: Decimal,
    pub currency: String,
    pub payment_mode: ExpensePaymentMode,
    pub reference: Option<String>,
    pub receipt_file_id: Option<Uuid>,
}

fn validate_amount(amount: Decimal) -> Result<(), ExpenseWriteError> {
    if amount < Decimal::ZERO {
        return Err(ExpenseWriteError::NegativeAmount);
    }
    Ok(())
}

fn validate_currency(currency: &str) -> Result<(), ExpenseWriteError> {
    if currency.len() != 3 || !currency.chars().all(|c| c.is_ascii_alphabetic()) {
        return Err(ExpenseWriteError::BadCurrency);
    }
    Ok(())
}

// ─── the service ──────────────────────────────────────────────────────────────

pub struct ExpensesWriteService {
    pool: PgPool,
    repo: ExpensesWriteRepository,
    approvals: RwLock<Arc<dyn ApprovalFiling>>,
    gl_sink: Arc<dyn GlPostSink>,
    reimbursements: Arc<dyn ReimbursementSink>,
}

impl ExpensesWriteService {
    pub fn new(pool: PgPool) -> Self {
        Self {
            pool,
            repo: ExpensesWriteRepository,
            approvals: RwLock::new(Arc::new(UnwiredApprovals)),
            gl_sink: Arc::new(super::expense_gl::UnwiredGlSink),
            reimbursements: Arc::new(UnwiredReimbursement),
        }
    }

    /// Snapshot of the approvals seam for a single call. The seam is a RwLock so a service
    /// the module already built (and handed to the router) can be re-armed by the composing
    /// app after the fact — the consuming builder below only helps hosts that construct the
    /// service themselves.
    fn approvals(&self) -> Arc<dyn ApprovalFiling> {
        self.approvals.read().expect("approvals seam lock").clone()
    }

    /// Wire the approvals seam BEFORE the service is handed anywhere (consuming builder).
    pub fn with_approvals(mut self, port: Arc<dyn ApprovalFiling>) -> Self {
        self.approvals = RwLock::new(port);
        self
    }

    /// Wire the approvals seam on a service the module already built and mounted. Idempotent:
    /// the last writer wins; callers normally arm exactly once at composition time.
    pub fn set_approvals(&self, port: Arc<dyn ApprovalFiling>) {
        *self.approvals.write().expect("approvals seam lock") = port;
    }

    /// Wire the GL seam (the app's adapter over accounting's PostingService, W2).
    pub fn with_gl_sink(mut self, sink: Arc<dyn GlPostSink>) -> Self {
        self.gl_sink = sink;
        self
    }

    /// Wire the reimbursement seam (the app's adapter over payment's create_payment, W2).
    pub fn with_reimbursement(mut self, sink: Arc<dyn ReimbursementSink>) -> Self {
        self.reimbursements = sink;
        self
    }

    /// One transaction with the AMBIENT request org scope relayed onto it, when the
    /// composing service bound one. The decorator-installed row-level fences evaluate
    /// for every statement of the verb. Transaction-local (`set_config(..., true)`):
    /// nothing leaks onto a pooled connection reused by the next request. Unfenced
    /// deployments have no ambient scope and skip this entirely.
    async fn scoped_tx(
        &self,
    ) -> Result<sqlx::Transaction<'static, sqlx::Postgres>, ExpenseWriteError> {
        let mut tx = self.pool.begin().await?;
        if let Some(scope) = org_scope::current_org_scope() {
            org_scope::bind_org_scope_on(&mut tx, &scope)
                .await
                .map_err(ExpenseWriteError::Db)?;
        }
        Ok(tx)
    }

    /// The company id for the seams that still key on one — the approvals filing, the GL
    /// envelope, and the reimbursement request. Sourced from the ambient org scope the
    /// COMPOSING service binds; absent → fail-closed. The module never guesses a company.
    fn legacy_company_id() -> Result<Uuid, ExpenseWriteError> {
        org_scope::current_org_scope()
            .and_then(|s| s.legacy_company_id())
            .ok_or(ExpenseWriteError::NoCompanyScope)
    }

    // ─── create / update ─────────────────────────────────────────────────────

    /// Create a draft claim. The category must exist (amount ≥ 0 and currency ISO-3 are
    /// checked here and again by the `expenses_amount_total_nonneg` CHECK at the DB).
    pub async fn create_expense(
        &self,
        claim: NewExpense,
        actor: Option<Uuid>,
    ) -> Result<Expense, ExpenseWriteError> {
        validate_amount(claim.amount_total)?;
        validate_currency(&claim.currency)?;

        let now = Utc::now();
        let mut tx = self.scoped_tx().await?;

        let category = self
            .repo
            .find_category(&mut tx, claim.category_id)
            .await?
            .ok_or(ExpenseWriteError::CategoryNotFound)?;

        let draft = Expense {
            id: Uuid::new_v4(),
            employee_id: claim.employee_id,
            category_id: category.id,
            expense_date: claim.expense_date,
            description: claim.description,
            amount_total: claim.amount_total,
            currency: claim.currency.to_ascii_uppercase(),
            payment_mode: claim.payment_mode,
            reference: claim.reference,
            approval_state: Default::default(),
            state: Default::default(),
            approval_request_id: None,
            journal_id: None,
            accounting_post_id: None,
            reimbursement_id: None,
            receipt_file_id: claim.receipt_file_id,
            metadata: Default::default(),
        };
        let created = self.repo.insert_expense(&mut tx, &draft, actor, now).await?;
        tx.commit().await?;
        Ok(created)
    }

    /// Draft-only field update with the ROW-TRUTH guard: whether the claim is editable is
    /// decided by the ROW's state (`WHERE approval_state='draft' AND state='draft'`), not by
    /// anything the payload carries. A submitted/refused claim matches zero rows → 409.
    pub async fn update_expense(
        &self,
        expense_id: Uuid,
        patch: ExpensePatch,
        actor: Option<Uuid>,
    ) -> Result<Expense, ExpenseWriteError> {
        if let Some(amount) = patch.amount_total {
            validate_amount(amount)?;
        }
        if let Some(currency) = patch.currency.as_deref() {
            validate_currency(currency)?;
        }
        let mut tx = self.scoped_tx().await?;
        // Re-validate the category when the claim is being reclassified; the ORIGINAL patch
        // (all fields, not just the category) is applied below either way.
        if let Some(category_id) = patch.category_id {
            self.repo
                .find_category(&mut tx, category_id)
                .await?
                .ok_or(ExpenseWriteError::CategoryNotFound)?;
        }
        let updated = self
            .repo
            .update_expense(&mut tx, expense_id, &patch, actor, Utc::now())
            .await?;
        tx.commit().await?;
        updated.ok_or(ExpenseWriteError::NotDraft)
    }

    // ─── lifecycle verbs ─────────────────────────────────────────────────────

    /// Submit a draft for approval. FILE-FIRST, outside the tx (the timeoff pattern: the port
    /// call is a network hop and must not hold a row lock). The link write is a compare-and-set
    /// on the very payload that was filed: if the draft was edited concurrently between the
    /// read and the write, the UPDATE matches zero rows and the caller gets a 409 — the row is
    /// never linked to a filing whose snapshot no longer describes it. There is NO background
    /// sweeper: a filing orphaned by such a race (or by a crash after `file` returns) simply
    /// stays pending in the engine until the requester withdraws it; a retried submit files
    /// idempotently (the engine returns the same live request for the resource) and converges
    /// on the fresh row. Unwired seam ⇒ the claim simply carries no link; a WIRED port that
    /// fails ⇒ the submit fails (no silently untracked claim). A wired port needs the legacy
    /// company twin for the filing (the engine's requests still key on one) — sourced off the
    /// ambient org scope, fail-closed.
    pub async fn submit_expense(
        &self,
        expense_id: Uuid,
        note: Option<String>,
        actor: Option<Uuid>,
    ) -> Result<Expense, ExpenseWriteError> {
        let expense = self.get_expense(expense_id).await?;
        if expense.approval_state != crate::domain::entity::ExpenseApprovalState::Draft {
            return Err(ExpenseWriteError::NotDraft);
        }
        // The category's caps, enforced at the door: one claim too big, or
        // the month's live claims (this employee + category) about to cross
        // the line. The scoped-fetch twin rides the request-dedicated
        // connection — a raw pool read runs unfenced and the caps vanish.
        let caps: Option<(Option<rust_decimal::Decimal>, Option<rust_decimal::Decimal>)> =
            backbone_orm::company_scope::fetch_optional_scoped(
                &self.pool,
                sqlx::query_as(
                    r#"SELECT cap_per_claim, cap_per_month FROM expenses.expense_categories
                        WHERE id = $1 AND (metadata->>'deleted_at') IS NULL"#,
                )
                .bind(expense.category_id),
            )
            .await?;
        if let Some((per_claim, per_month)) = caps {
            if let Some(cap) = per_claim {
                if expense.amount_total > cap {
                    return Err(ExpenseWriteError::CategoryCapExceeded {
                        kind: "per claim",
                        cap,
                        amount: expense.amount_total,
                    });
                }
            }
            if let Some(cap) = per_month {
                let month_start = chrono::NaiveDate::from_ymd_opt(
                    expense.expense_date.year(),
                    expense.expense_date.month(),
                    1,
                )
                .unwrap_or(expense.expense_date);
                let spent: rust_decimal::Decimal =
                    backbone_orm::company_scope::fetch_optional_scoped(
                        &self.pool,
                        sqlx::query_as::<_, (rust_decimal::Decimal,)>(
                            r#"SELECT COALESCE(SUM(amount_total), 0) FROM expenses.expenses
                                WHERE employee_id = $1 AND category_id = $2
                                  AND expense_date >= $3 AND expense_date < $3 + interval '1 month'
                                  AND approval_state IN ('draft', 'submitted', 'approved')
                                  AND id <> $4
                                  AND (metadata->>'deleted_at') IS NULL"#,
                        )
                        .bind(expense.employee_id)
                        .bind(expense.category_id)
                        .bind(month_start)
                        .bind(expense_id),
                    )
                    .await?
                    .map(|(s,)| s)
                    .unwrap_or_default();
                if spent + expense.amount_total > cap {
                    return Err(ExpenseWriteError::CategoryCapExceeded {
                        kind: "per month",
                        cap,
                        amount: spent + expense.amount_total,
                    });
                }
            }
        }

        let filing = ExpenseApprovalFilingRequest {
            company_id: Self::legacy_company_id()?,
            expense_id,
            employee_id: expense.employee_id,
            category_id: expense.category_id,
            expense_date: expense.expense_date,
            amount_total: expense.amount_total,
            currency: expense.currency.clone(),
            description: expense.description.clone(),
            note,
            submitted_at: Utc::now(),
        };
        let approval_request_id = match self.approvals().file(&filing).await {
            Ok(id) => Some(id),
            Err(ApprovalSeamError::Unwired) => None,
            Err(e) => return Err(e.into()),
        };

        let mut tx = self.scoped_tx().await?;
        let submitted = self
            .repo
            .mark_submitted(
                &mut tx,
                expense_id,
                approval_request_id,
                actor,
                Utc::now(),
                filing.employee_id,
                filing.category_id,
                filing.expense_date,
                filing.amount_total,
                &filing.currency,
            )
            .await?
            .ok_or(ExpenseWriteError::NotDraft)?;
        tx.commit().await?;
        Ok(submitted)
    }

    /// Approve a submitted claim — TR2 FAIL-CLOSED. If the claim is linked into the approvals
    /// engine (`approval_request_id` set), it is granted ONLY by an `Approved` verdict; any
    /// other verdict, an unwired port, or an unknown request ⇒ 409 — the engine a claim was
    /// filed into is never bypassed. An unlinked claim (unwired deployment) approves directly,
    /// exactly as timeoff P1.
    pub async fn approve_expense(
        &self,
        expense_id: Uuid,
        actor: Option<Uuid>,
    ) -> Result<Expense, ExpenseWriteError> {
        let expense = self.get_expense(expense_id).await?;
        if expense.approval_state != crate::domain::entity::ExpenseApprovalState::Submitted {
            return Err(ExpenseWriteError::NotSubmitted);
        }
        if let Some(approval_request_id) = expense.approval_request_id {
            match self.approvals().status(approval_request_id).await {
                Ok(ApprovalVerdict::Approved) => {}
                Ok(_) => return Err(ExpenseWriteError::ApprovalNotGranted),
                Err(ApprovalSeamError::Unwired)
                | Err(ApprovalSeamError::UnknownApprovalRequest(_)) => {
                    return Err(ExpenseWriteError::ApprovalNotGranted);
                }
                Err(e) => return Err(e.into()),
            }
        }

        let mut tx = self.scoped_tx().await?;
        let approved = self
            .repo
            .mark_approved(&mut tx, expense_id, actor, Utc::now())
            .await?
            .ok_or(ExpenseWriteError::NotSubmitted)?;
        tx.commit().await?;
        Ok(approved)
    }

    /// Refuse a submitted claim (manager verb — sticky, terminal unless re-filed as a new
    /// claim). The reason is kept in the audit metadata for the report projection.
    pub async fn refuse_expense(
        &self,
        expense_id: Uuid,
        reason: Option<&str>,
        actor: Option<Uuid>,
    ) -> Result<Expense, ExpenseWriteError> {
        let mut tx = self.scoped_tx().await?;
        let refused = self
            .repo
            .mark_refused(&mut tx, expense_id, reason, actor, Utc::now())
            .await?
            .ok_or(ExpenseWriteError::NotSubmitted)?;
        tx.commit().await?;
        Ok(refused)
    }

    /// Post an approved claim to the GL — ONE envelope per expense (HEM-13), built from the
    /// category's expense account + the tax overlay, asserted balanced, then sent through the
    /// sink OUTSIDE the tx. On the ack, `journal_id` + `accounting_post_id` are stamped and
    /// state → `posted`. The stable idempotency key (`expense:{company}:{id}`, the company
    /// being the seams' legacy twin) + the `state='approved'` row guard make a double post
    /// either reuse accounting's dedup or fail 409 here — never a double entry. Unwired sink
    /// ⇒ 422 `gl_post_rejected` with the sink's `gl_seam_unwired` code; the row stays approved
    /// and retryable (accounting lands W2).
    pub async fn post_expense(
        &self,
        expense_id: Uuid,
        accounts: PostAccounts,
        actor: Option<Uuid>,
    ) -> Result<Expense, ExpenseWriteError> {
        let expense = self.get_expense(expense_id).await?;
        if expense.accounting_post_id.is_some() || expense.state == crate::domain::entity::ExpenseState::Posted
            || expense.state == crate::domain::entity::ExpenseState::Done
        {
            return Err(ExpenseWriteError::AlreadyPosted);
        }
        if expense.approval_state != crate::domain::entity::ExpenseApprovalState::Approved
            || expense.state != crate::domain::entity::ExpenseState::Approved
        {
            return Err(ExpenseWriteError::NotApprovedForPost);
        }

        let (category, tax_rows) = {
            let mut tx = self.scoped_tx().await?;
            let category = self
                .repo
                .find_category(&mut tx, expense.category_id)
                .await?
                .ok_or(ExpenseWriteError::CategoryNotFound)?;
            let tax_rows = self.repo.tax_lines_for(&mut tx, expense_id).await?;
            tx.commit().await?;
            (category, tax_rows)
        };

        let tax_lines: Vec<TaxLineInput> = tax_rows
            .iter()
            .map(|t| TaxLineInput {
                basis: t.basis.clone(),
                account_id: t.account_id,
                tax_amount: t.tax_amount,
            })
            .collect();
        // The GL envelope still keys on a company (accounting's books + idempotency dedup are
        // unstripped): source the legacy twin off the ambient org scope, fail-closed.
        let company_id = Self::legacy_company_id()?;
        let envelope =
            build_expense_envelope(company_id, &expense, &category, &tax_lines, &accounts)
                .map_err(|e| ExpenseWriteError::Internal(format!("envelope: {e}")))?;
        if !envelope.is_balanced() {
            return Err(ExpenseWriteError::Internal(
                "expense envelope does not balance — refusing to send".to_string(),
            ));
        }

        let ack = self.gl_sink.post(&envelope).await.map_err(|rej| {
            ExpenseWriteError::GlRejected {
                code: rej.code,
                message: rej.message,
            }
        })?;

        let mut tx = self.scoped_tx().await?;
        let posted = self
            .repo
            .mark_posted(&mut tx, expense_id, ack.journal_id, ack.post_id, actor, Utc::now())
            .await?
            .ok_or(ExpenseWriteError::AlreadyPosted)?;
        tx.commit().await?;
        Ok(posted)
    }

    /// Settle a posted own-account claim: the reimbursement sink pays the employee, the ack id
    /// stamps `reimbursement_id`, state → `done`. Company-account claims settle at the bank the
    /// moment they post — `settle` refuses them (409). Sink call outside the tx; unwired ⇒
    /// fails closed (payment composes W2). The sink request needs the legacy company twin (the
    /// payment books still key on one) — sourced off the ambient org scope, fail-closed.
    pub async fn settle_expense(
        &self,
        expense_id: Uuid,
        actor: Option<Uuid>,
    ) -> Result<Expense, ExpenseWriteError> {
        let expense = self.get_expense(expense_id).await?;
        if expense.state != crate::domain::entity::ExpenseState::Posted {
            return Err(ExpenseWriteError::NotPosted);
        }
        if expense.payment_mode == ExpensePaymentMode::CompanyAccount {
            return Err(ExpenseWriteError::NotReimbursable);
        }

        let (gross, withholding) = {
            let mut tx = self.scoped_tx().await?;
            let rows = self.repo.tax_lines_for(&mut tx, expense_id).await?;
            tx.commit().await?;
            rows.iter().fold(
                (expense.amount_total, Decimal::ZERO),
                |(g, w), t| match t.basis.as_str() {
                    "input" => (g + t.tax_amount, w),
                    _ => (g, w + t.tax_amount),
                },
            )
        };

        let request = ReimbursementRequest {
            company_id: Self::legacy_company_id()?,
            expense_id,
            employee_id: expense.employee_id,
            amount: gross - withholding,
            currency: expense.currency.clone(),
            reference: expense.reference.clone(),
        };
        let ack = self.reimbursements.reimburse(&request).await?;

        let mut tx = self.scoped_tx().await?;
        let settled = self
            .repo
            .mark_settled(&mut tx, expense_id, ack.payment_id, actor, Utc::now())
            .await?
            .ok_or(ExpenseWriteError::NotPosted)?;
        tx.commit().await?;
        Ok(settled)
    }

    // ─── evidence + overlay ──────────────────────────────────────────────────

    /// Attach a receipt scan (plain uuid → bucket file; the bucket tags
    /// `owner_module=expenses` on its side). Open claims only (draft/submitted).
    pub async fn attach_receipt(
        &self,
        expense_id: Uuid,
        receipt_file_id: Uuid,
        actor: Option<Uuid>,
    ) -> Result<Expense, ExpenseWriteError> {
        self.set_receipt(expense_id, Some(receipt_file_id), actor)
            .await
    }

    /// Detach the receipt scan. Open claims only.
    pub async fn detach_receipt(
        &self,
        expense_id: Uuid,
        actor: Option<Uuid>,
    ) -> Result<Expense, ExpenseWriteError> {
        self.set_receipt(expense_id, None, actor).await
    }

    /// Shared receipt writer: open claims only (draft/submitted) — once decided, the
    /// evidence set is fixed.
    async fn set_receipt(
        &self,
        expense_id: Uuid,
        receipt_file_id: Option<Uuid>,
        actor: Option<Uuid>,
    ) -> Result<Expense, ExpenseWriteError> {
        let mut tx = self.scoped_tx().await?;
        let updated = self
            .repo
            .set_receipt(&mut tx, expense_id, receipt_file_id, actor, Utc::now())
            .await?
            .ok_or(ExpenseWriteError::NotFound)?;
        tx.commit().await?;
        Ok(updated)
    }

    /// Replace the tax overlay (draft-only — the overlay is frozen at submit so the approver
    /// and the GL always see the same lines). Basis ∈ input|withholding; amounts ≥ 0
    /// (DB-CHECKed); the whole replace is one transaction.
    pub async fn set_tax_lines(
        &self,
        expense_id: Uuid,
        lines: Vec<TaxLineWrite>,
        actor: Option<Uuid>,
    ) -> Result<Expense, ExpenseWriteError> {
        for line in &lines {
            if line.basis != "input" && line.basis != "withholding" {
                return Err(ExpenseWriteError::BadTaxBasis);
            }
            validate_amount(line.tax_amount)?;
        }

        let now = Utc::now();
        let mut tx = self.scoped_tx().await?;
        let expense = self
            .repo
            .get_expense(&mut tx, expense_id)
            .await?
            .ok_or(ExpenseWriteError::NotFound)?;
        if expense.approval_state != crate::domain::entity::ExpenseApprovalState::Draft {
            return Err(ExpenseWriteError::NotDraft);
        }
        // Row-truth draft guard INSIDE the write (council F1): the overlay statements
        // themselves carry the parent's (draft, draft) predicate, so a submit that commits
        // mid-replace makes them match zero rows — NotDraft rolls the whole tx back, never a
        // mutated overlay on a submitted claim.
        let inserted = self
            .repo
            .replace_tax_lines(&mut tx, expense_id, &lines, actor, now)
            .await?;
        if inserted != lines.len() as u64 {
            return Err(ExpenseWriteError::NotDraft);
        }
        let expense = self
            .repo
            .get_expense(&mut tx, expense_id)
            .await?
            .ok_or(ExpenseWriteError::NotFound)?;
        tx.commit().await?;
        Ok(expense)
    }

    // ─── reads ───────────────────────────────────────────────────────────────

    /// One live claim (row-truth read; under a decorated deployment the row-level fences
    /// decide visibility — an out-of-scope id simply matches zero rows ⇒ 404).
    pub async fn get_expense(&self, expense_id: Uuid) -> Result<Expense, ExpenseWriteError> {
        let mut tx = self.scoped_tx().await?;
        let expense = self
            .repo
            .get_expense(&mut tx, expense_id)
            .await?
            .ok_or(ExpenseWriteError::NotFound)?;
        tx.commit().await?;
        Ok(expense)
    }

    /// The report projection (HEM-13 — grouping is read-only SQL, never an entity): totals per
    /// employee × category × state over `[from, to]`, optionally one employee.
    pub async fn report(
        &self,
        employee_id: Option<Uuid>,
        from: NaiveDate,
        to: NaiveDate,
    ) -> Result<Vec<ExpenseReportRow>, ExpenseWriteError> {
        if from > to {
            return Err(ExpenseWriteError::BadDateRange);
        }
        let mut tx = self.scoped_tx().await?;
        let rows = self
            .repo
            .report(&mut tx, employee_id, from, to)
            .await?;
        tx.commit().await?;
        Ok(rows)
    }
}
