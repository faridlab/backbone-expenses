//! The approvals seam (Wave 1 P3, H-4) — the port trait the composing app
//! implements against backbone-approvals once the H-9 decision engine lands.
//!
//! ADR-0004: shipped libraries keep ZERO normal Cargo edges on each other, so
//! expenses cannot depend on the approvals crate. The link is data + behavior:
//! `expenses.approval_request_id` (a logical FK, no DB constraint across module
//! schemas) + this port, supplied at composition time.
//!
//! Mirrors backbone-timeoff's P1 seam verbatim in shape. NOTE: the approvals
//! engine's `ApprovalResourceType` has no `expense` variant yet — the engine
//! side is a Wave 1 P6 concern; the port already speaks the right vocabulary
//! (`file` + `status`) so wiring it later is app-side only.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// The verdict on a filed approval, as read back through the port. Deliberately
/// a mirror of the engine's status vocabulary restricted to what the expense
/// verbs need — the engine's richer states (escalated, delegated, …) all read
/// as "not yet approved" from here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalVerdict {
    /// Awaiting a decision.
    Pending,
    /// Granted.
    Approved,
    /// Refused (sticky — the engine does not re-ask).
    Rejected,
    /// Withdrawn by the requester.
    Cancelled,
}

/// What expenses files for approval: WHO claimed WHAT for HOW MUCH, plus the
/// back-reference so the engine's notifications link back to the claim.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExpenseApprovalFilingRequest {
    /// The company scope (stamped onto the ApprovalRequest for its own fence).
    pub company_id: Uuid,
    /// The expense claim the filing is about (correlation id).
    pub expense_id: Uuid,
    /// The claimant # logical FK to employee.Employee.id.
    pub employee_id: Uuid,
    /// The expense classification # logical FK to expenses.ExpenseCategory.id.
    pub category_id: Uuid,
    /// The date the expense was incurred.
    pub expense_date: chrono::NaiveDate,
    /// Total claimed amount excluding tax (what the approver sees).
    pub amount_total: rust_decimal::Decimal,
    /// Claim currency (single-currency v1: IDR).
    pub currency: String,
    /// What the expense was for.
    pub description: String,
    /// Claimant note, if any.
    pub note: Option<String>,
    /// When the claim was submitted.
    pub submitted_at: DateTime<Utc>,
}

/// Errors from the approvals seam. `Unwired` is the load-bearing variant: it is
/// what the default [`UnwiredApprovals`] returns, and what `approve` converts
/// into a fail-closed `approval_not_granted` when a claim carries an
/// `approval_request_id` but no port is wired (TR2).
#[derive(Debug, thiserror::Error)]
pub enum ApprovalSeamError {
    #[error("the approvals seam is not wired — supply an ApprovalFiling port to use linked approvals")]
    Unwired,
    #[error("approval request {0} not found on the approvals side")]
    UnknownApprovalRequest(Uuid),
    #[error("approvals port transport error: {0}")]
    Transport(String),
}

/// The port (ADR-0004 serialized-port pattern). Implemented by the composing
/// app against backbone-approvals; `expenses` only ever speaks this trait.
#[async_trait::async_trait]
pub trait ApprovalFiling: Send + Sync {
    /// File a new approval request for a submitted expense claim; returns
    /// the created `approvals.ApprovalRequest.id` to stamp onto
    /// `expenses.approval_request_id`.
    async fn file(&self, req: &ExpenseApprovalFilingRequest) -> Result<Uuid, ApprovalSeamError>;

    /// Read back the verdict for a previously filed approval.
    async fn status(&self, approval_request_id: Uuid) -> Result<ApprovalVerdict, ApprovalSeamError>;
}

/// The default port: nothing is wired. Filing fails loudly (a caller asking for
/// tracked approvals without wiring the engine gets an explicit error, not a
/// silently untracked claim); status lookups fail closed for the same reason.
pub struct UnwiredApprovals;

#[async_trait::async_trait]
impl ApprovalFiling for UnwiredApprovals {
    async fn file(&self, _req: &ExpenseApprovalFilingRequest) -> Result<Uuid, ApprovalSeamError> {
        Err(ApprovalSeamError::Unwired)
    }

    async fn status(&self, approval_request_id: Uuid) -> Result<ApprovalVerdict, ApprovalSeamError> {
        Err(ApprovalSeamError::UnknownApprovalRequest(approval_request_id))
    }
}
