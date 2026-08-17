//! The reimbursement seam (Wave 1 P3, H-4) — the port the composing app
//! implements over backbone-payment's `PaymentWriteService::create_payment`
//! when the finance wave (W2) composes payment into serpa.
//!
//! ADR-0004: shipped libraries keep ZERO normal Cargo edges on each other, and
//! backbone-payment exposes NO write port of its own (its write path is a
//! direct `create_payment(NewPayment)` service call). So expenses declares the
//! port it needs — mirroring `GlPostSink`'s shape — and the app implements it
//! at composition time. Until then [`UnwiredReimbursement`] is the default and
//! `settle` fails CLOSED: no expense may reach `done` without a real payment
//! behind the id.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// What expenses asks to be reimbursed: one own-account claim, already posted
/// to the GL, now due to the employee.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReimbursementRequest {
    /// The company scope (stamped onto the payment for its own fence).
    pub company_id: Uuid,
    /// The posted expense the payment settles (correlation id).
    pub expense_id: Uuid,
    /// The payee # logical FK to employee.Employee.id.
    pub employee_id: Uuid,
    /// Net amount due (claim + input tax − withholding, as posted).
    pub amount: rust_decimal::Decimal,
    /// Claim currency (single-currency v1: IDR).
    pub currency: String,
    /// Supplier document number / receipt ref, if the claim carried one.
    pub reference: Option<String>,
}

/// The payment side's ack: the created `payment.payment.id` stamped onto
/// `expenses.reimbursement_id`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReimbursementAck {
    pub payment_id: Uuid,
}

/// Errors from the reimbursement seam. `Unwired` is the load-bearing variant:
/// the default [`UnwiredReimbursement`] returns it, and `settle` maps it to a
/// fail-closed 422 rather than a silent skip.
#[derive(Debug, thiserror::Error)]
pub enum ReimbursementSeamError {
    #[error("the reimbursement seam is not wired — supply a ReimbursementSink to settle expenses")]
    Unwired,
    #[error("reimbursement port transport error: {0}")]
    Transport(String),
}

/// The port (ADR-0004 serialized-port pattern). Implemented by the composing
/// app over backbone-payment's write service; `expenses` only ever speaks this
/// trait.
#[async_trait::async_trait]
pub trait ReimbursementSink: Send + Sync {
    async fn reimburse(
        &self,
        req: &ReimbursementRequest,
    ) -> Result<ReimbursementAck, ReimbursementSeamError>;
}

/// The default port: nothing is wired (payment composes in W2). Settle fails
/// loudly — an explicit error, never a claim marked `done` with no payment.
pub struct UnwiredReimbursement;

#[async_trait::async_trait]
impl ReimbursementSink for UnwiredReimbursement {
    async fn reimburse(
        &self,
        _req: &ReimbursementRequest,
    ) -> Result<ReimbursementAck, ReimbursementSeamError> {
        Err(ReimbursementSeamError::Unwired)
    }
}
