//! Guarded route composition — the RECOMMENDED way to mount the expenses module.
//!
//! Hand-authored (user-owned; see `metaphor.codegen.yaml`). Closes the CRUD-bypass: the generated
//! 12-endpoint CRUD surface would let a well-formed request write `state='posted'` directly or
//! soft-delete a referenced category out from under live claims. Here:
//!
//! - **Expense reads**: GETs only. The lifecycle fields are writable solely through the verbs.
//! - **Category CRUD**: master data — full generated CRUD (admin surface; unique-code conflicts
//!   surface as the handler's standard error shape).
//! - **Tax-line reads**: GETs only — the overlay is mutable solely through `set_tax_lines`
//!   (draft-only, frozen at submit).
//! - **Writes**: every claim mutation goes through [`ExpensesWriteService`], which owns the
//!   two-field lifecycle (ADR-0016), the row-truth state guards, the TR2 fail-closed approve,
//!   and the three seams (approvals / GL / reimbursement — all default-unwired, all fail closed).
//!
//! The tenant comes from the [`CompanyContext`] the `company_auth` middleware inserts — never
//! from the body. `postAccounts` on the POST body is the W1 stopgap: accounting (W2) will supply
//! the payable/bank accounts at composition time instead of the caller.
//!
//! **Fence posture** (ADR-0008): the generated GET read routes and the category CRUD carry no
//! company predicate in SQL — their row visibility is the DB fence (strict RLS, `app.company_id`
//! request binding), exactly like the family's other guarded compositions. Composers MUST mount
//! this behind `company_auth` with the request-scoped DB binding (the serpa posture), where a
//! cross-tenant id simply matches zero rows. Every verb's SQL additionally carries its own
//! company predicate (belt-and-braces), so the write path 404s cross-tenant even on an unfenced
//! connection — pinned by tests/integrity_probes.rs.

use std::sync::Arc;

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{delete, post},
    Json, Router,
};
use backbone_auth::company::CompanyContext;
use chrono::NaiveDate;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::application::service::expense_gl::PostAccounts;
use crate::application::service::expenses_write_service::{
    ExpenseWriteError, ExpensesWriteService, NewExpense,
};
use crate::domain::entity::{Expense, ExpensePaymentMode};
use crate::infrastructure::persistence::{ExpensePatch, ExpenseReportRow, TaxLineWrite};
use crate::ExpensesModule;

use super::{
    create_expense_category_routes, create_expense_read_routes, create_expense_tax_line_read_routes,
};

#[derive(Debug, Serialize)]
struct ErrorBody {
    error: &'static str,
    message: String,
}

fn err_response(e: ExpenseWriteError) -> axum::response::Response {
    let status = StatusCode::from_u16(e.http_status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    (
        status,
        Json(ErrorBody {
            error: e.code(),
            message: e.to_string(),
        }),
    )
        .into_response()
}

/// The HTTP shape of a claim row — camelCase, lifecycle fields included (read-only proof of
/// the verb's effect: `approvalState`, `state`, the GL/reimbursement stamps).
fn expense_response(status: StatusCode, expense: &Expense) -> axum::response::Response {
    (status, Json(ExpenseBody::from(expense))).into_response()
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ExpenseBody<'a> {
    id: Uuid,
    company_id: Uuid,
    employee_id: Uuid,
    category_id: Uuid,
    expense_date: NaiveDate,
    description: &'a str,
    amount_total: Decimal,
    currency: &'a str,
    payment_mode: &'a str,
    reference: &'a Option<String>,
    approval_state: &'a str,
    state: &'a str,
    approval_request_id: Option<Uuid>,
    journal_id: Option<Uuid>,
    accounting_post_id: Option<Uuid>,
    reimbursement_id: Option<Uuid>,
    receipt_file_id: Option<Uuid>,
}

impl<'a> From<&'a Expense> for ExpenseBody<'a> {
    fn from(e: &'a Expense) -> Self {
        Self {
            id: e.id,
            company_id: e.company_id,
            employee_id: e.employee_id,
            category_id: e.category_id,
            expense_date: e.expense_date,
            description: &e.description,
            amount_total: e.amount_total,
            currency: &e.currency,
            payment_mode: match e.payment_mode {
                ExpensePaymentMode::OwnAccount => "own_account",
                ExpensePaymentMode::CompanyAccount => "company_account",
            },
            reference: &e.reference,
            approval_state: match e.approval_state {
                crate::domain::entity::ExpenseApprovalState::Draft => "draft",
                crate::domain::entity::ExpenseApprovalState::Submitted => "submitted",
                crate::domain::entity::ExpenseApprovalState::Approved => "approved",
                crate::domain::entity::ExpenseApprovalState::Refused => "refused",
            },
            state: match e.state {
                crate::domain::entity::ExpenseState::Draft => "draft",
                crate::domain::entity::ExpenseState::Submitted => "submitted",
                crate::domain::entity::ExpenseState::Approved => "approved",
                crate::domain::entity::ExpenseState::Posted => "posted",
                crate::domain::entity::ExpenseState::Done => "done",
                crate::domain::entity::ExpenseState::Refused => "refused",
            },
            approval_request_id: e.approval_request_id,
            journal_id: e.journal_id,
            accounting_post_id: e.accounting_post_id,
            reimbursement_id: e.reimbursement_id,
            receipt_file_id: e.receipt_file_id,
        }
    }
}

/// The acting principal as a uuid actor stamp, when the token's `sub` parses as one.
fn actor(t: &CompanyContext) -> Option<Uuid> {
    Uuid::parse_str(&t.user_id).ok()
}

// ── request bodies ─────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateExpenseBody {
    employee_id: Uuid,
    category_id: Uuid,
    expense_date: NaiveDate,
    description: String,
    amount_total: Decimal,
    #[serde(default = "default_currency")]
    currency: String,
    #[serde(default)]
    payment_mode: Option<String>, // "own_account" (default) | "company_account"
    #[serde(default)]
    reference: Option<String>,
    #[serde(default)]
    receipt_file_id: Option<Uuid>,
}

fn default_currency() -> String {
    "IDR".to_string()
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateExpenseBody {
    #[serde(default)]
    category_id: Option<Uuid>,
    #[serde(default)]
    expense_date: Option<NaiveDate>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    amount_total: Option<Decimal>,
    #[serde(default)]
    currency: Option<String>,
    #[serde(default)]
    payment_mode: Option<String>, // "own_account" | "company_account"
    #[serde(default, deserialize_with = "deserialize_opt_option")]
    reference: Option<Option<String>>,
}

/// `reference: null` clears the field; absent leaves it untouched (serde's double-Option).
fn deserialize_opt_option<'de, D>(de: D) -> Result<Option<Option<String>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Some(Option::deserialize(de)?))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SubmitBody {
    #[serde(default)]
    note: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RefuseBody {
    #[serde(default)]
    reason: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PostBody {
    /// W1 stopgap: the host/caller resolves the payable + bank accounts until accounting (W2)
    /// supplies them at composition time.
    post_accounts: PostAccountsBody,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PostAccountsBody {
    employee_payable_account_id: Uuid,
    bank_account_id: Uuid,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReceiptBody {
    receipt_file_id: Uuid,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TaxLinesBody {
    lines: Vec<TaxLineBody>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TaxLineBody {
    basis: String, // "input" | "withholding"
    account_id: Uuid,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    rate: Decimal,
    tax_amount: Decimal,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReportQuery {
    #[serde(default)]
    employee_id: Option<Uuid>,
    from: NaiveDate,
    to: NaiveDate,
}

fn parse_payment_mode(s: Option<&str>) -> Option<ExpensePaymentMode> {
    match s {
        None | Some("own_account") => Some(ExpensePaymentMode::OwnAccount),
        Some("company_account") => Some(ExpensePaymentMode::CompanyAccount),
        Some(_) => None,
    }
}

// ── handlers ───────────────────────────────────────────────────────────────────

async fn create_expense(
    State(svc): State<Arc<ExpensesWriteService>>,
    tenant: CompanyContext,
    Json(b): Json<CreateExpenseBody>,
) -> axum::response::Response {
    let Some(payment_mode) = parse_payment_mode(b.payment_mode.as_deref()) else {
        return err_response(ExpenseWriteError::BadPaymentMode);
    };
    match svc
        .create_expense(
            tenant.company_id,
            NewExpense {
                employee_id: b.employee_id,
                category_id: b.category_id,
                expense_date: b.expense_date,
                description: b.description,
                amount_total: b.amount_total,
                currency: b.currency,
                payment_mode,
                reference: b.reference,
                receipt_file_id: b.receipt_file_id,
            },
            actor(&tenant),
        )
        .await
    {
        Ok(expense) => expense_response(StatusCode::CREATED, &expense),
        Err(e) => err_response(e),
    }
}

async fn update_expense(
    State(svc): State<Arc<ExpensesWriteService>>,
    tenant: CompanyContext,
    Path(expense_id): Path<Uuid>,
    Json(b): Json<UpdateExpenseBody>,
) -> axum::response::Response {
    // Omitted payment_mode = leave unchanged (unlike create, where it defaults to own_account).
    let payment_mode = match b.payment_mode.as_deref() {
        None => None,
        Some(s) => match parse_payment_mode(Some(s)) {
            Some(mode) => Some(mode.to_string()),
            None => return err_response(ExpenseWriteError::BadPaymentMode),
        },
    };
    let patch = ExpensePatch {
        category_id: b.category_id,
        expense_date: b.expense_date,
        description: b.description,
        amount_total: b.amount_total,
        currency: b.currency,
        payment_mode,
        reference: b.reference,
    };
    match svc
        .update_expense(tenant.company_id, expense_id, patch, actor(&tenant))
        .await
    {
        Ok(expense) => expense_response(StatusCode::OK, &expense),
        Err(e) => err_response(e),
    }
}

async fn submit_expense(
    State(svc): State<Arc<ExpensesWriteService>>,
    tenant: CompanyContext,
    Path(expense_id): Path<Uuid>,
    body: Option<Json<SubmitBody>>,
) -> axum::response::Response {
    let note = body.and_then(|Json(b)| b.note);
    match svc
        .submit_expense(tenant.company_id, expense_id, note, actor(&tenant))
        .await
    {
        Ok(expense) => expense_response(StatusCode::OK, &expense),
        Err(e) => err_response(e),
    }
}

async fn approve_expense(
    State(svc): State<Arc<ExpensesWriteService>>,
    tenant: CompanyContext,
    Path(expense_id): Path<Uuid>,
) -> axum::response::Response {
    match svc
        .approve_expense(tenant.company_id, expense_id, actor(&tenant))
        .await
    {
        Ok(expense) => expense_response(StatusCode::OK, &expense),
        Err(e) => err_response(e),
    }
}

async fn refuse_expense(
    State(svc): State<Arc<ExpensesWriteService>>,
    tenant: CompanyContext,
    Path(expense_id): Path<Uuid>,
    body: Option<Json<RefuseBody>>,
) -> axum::response::Response {
    let reason = body.and_then(|Json(b)| b.reason);
    match svc
        .refuse_expense(tenant.company_id, expense_id, reason.as_deref(), actor(&tenant))
        .await
    {
        Ok(expense) => expense_response(StatusCode::OK, &expense),
        Err(e) => err_response(e),
    }
}

async fn post_expense(
    State(svc): State<Arc<ExpensesWriteService>>,
    tenant: CompanyContext,
    Path(expense_id): Path<Uuid>,
    Json(b): Json<PostBody>,
) -> axum::response::Response {
    let accounts = PostAccounts {
        employee_payable_account_id: b.post_accounts.employee_payable_account_id,
        bank_account_id: b.post_accounts.bank_account_id,
    };
    match svc
        .post_expense(tenant.company_id, expense_id, accounts, actor(&tenant))
        .await
    {
        Ok(expense) => expense_response(StatusCode::OK, &expense),
        Err(e) => err_response(e),
    }
}

async fn settle_expense(
    State(svc): State<Arc<ExpensesWriteService>>,
    tenant: CompanyContext,
    Path(expense_id): Path<Uuid>,
) -> axum::response::Response {
    match svc
        .settle_expense(tenant.company_id, expense_id, actor(&tenant))
        .await
    {
        Ok(expense) => expense_response(StatusCode::OK, &expense),
        Err(e) => err_response(e),
    }
}

async fn attach_receipt(
    State(svc): State<Arc<ExpensesWriteService>>,
    tenant: CompanyContext,
    Path(expense_id): Path<Uuid>,
    Json(b): Json<ReceiptBody>,
) -> axum::response::Response {
    match svc
        .attach_receipt(tenant.company_id, expense_id, b.receipt_file_id, actor(&tenant))
        .await
    {
        Ok(expense) => expense_response(StatusCode::OK, &expense),
        Err(e) => err_response(e),
    }
}

async fn detach_receipt(
    State(svc): State<Arc<ExpensesWriteService>>,
    tenant: CompanyContext,
    Path(expense_id): Path<Uuid>,
) -> axum::response::Response {
    match svc
        .detach_receipt(tenant.company_id, expense_id, actor(&tenant))
        .await
    {
        Ok(expense) => expense_response(StatusCode::OK, &expense),
        Err(e) => err_response(e),
    }
}

async fn set_tax_lines(
    State(svc): State<Arc<ExpensesWriteService>>,
    tenant: CompanyContext,
    Path(expense_id): Path<Uuid>,
    Json(b): Json<TaxLinesBody>,
) -> axum::response::Response {
    let lines = b
        .lines
        .into_iter()
        .map(|l| TaxLineWrite {
            basis: l.basis,
            account_id: l.account_id,
            description: l.description,
            rate: l.rate,
            tax_amount: l.tax_amount,
        })
        .collect();
    match svc
        .set_tax_lines(tenant.company_id, expense_id, lines, actor(&tenant))
        .await
    {
        Ok(expense) => expense_response(StatusCode::OK, &expense),
        Err(e) => err_response(e),
    }
}

async fn report(
    State(svc): State<Arc<ExpensesWriteService>>,
    tenant: CompanyContext,
    Query(q): Query<ReportQuery>,
) -> axum::response::Response {
    #[derive(Debug, Serialize)]
    #[serde(rename_all = "camelCase")]
    struct ReportResponse {
        from: NaiveDate,
        to: NaiveDate,
        rows: Vec<ExpenseReportRow>,
    }
    match svc
        .report(tenant.company_id, q.employee_id, q.from, q.to)
        .await
    {
        Ok(rows) => (
            StatusCode::OK,
            Json(ReportResponse {
                from: q.from,
                to: q.to,
                rows,
            }),
        )
            .into_response(),
        Err(e) => err_response(e),
    }
}

// ── composition ────────────────────────────────────────────────────────────────

/// Build the guarded expenses router: validated claim verbs + report projection + category
/// CRUD + safe reads, NO generic expense/tax-line mutation. Mount under the host's
/// authenticated (`company_auth`) tree.
pub fn create_guarded_expenses_routes(m: &ExpensesModule) -> Router {
    let writes = Router::new()
        .route("/expenses", post(create_expense))
        .route("/expenses/:id", axum::routing::patch(update_expense))
        .route("/expenses/:id/submit", post(submit_expense))
        .route("/expenses/:id/approve", post(approve_expense))
        .route("/expenses/:id/refuse", post(refuse_expense))
        .route("/expenses/:id/post", post(post_expense))
        .route("/expenses/:id/settle", post(settle_expense))
        .route("/expenses/:id/receipt", post(attach_receipt))
        .route("/expenses/:id/receipt", delete(detach_receipt))
        .route("/expenses/:id/tax-lines", axum::routing::put(set_tax_lines))
        .route("/expenses/report", axum::routing::get(report))
        .with_state(m.expenses_write_service.clone());

    // Reads: claim GETs + overlay GETs. Categories mount full CRUD (master data, the admin
    // surface). Generic expense/tax-line WRITES are deliberately absent — every claim mutation
    // flows through the verbs above.
    Router::new()
        .merge(create_expense_read_routes(m.expense_service.clone()))
        .merge(create_expense_category_routes(m.expense_category_service.clone()))
        .merge(create_expense_tax_line_read_routes(m.expense_tax_line_service.clone()))
        .merge(writes)
}
