//! Outbound GL-posting port (hand-authored, user-owned) — re-export of the shared contract
//! plus the expense envelope builder.
//!
//! The GL-posting wire types (`AccountingPostEnvelope`, `GlPostLine`, `GlPostAck`, `GlPostRejected`)
//! and the `GlPostSink` port live in the shared `backbone-gl-posting` crate (backbone-framework
//! v2.7.5+) — the single source for all producers. This file re-exports them (billing's
//! `billing_gl.rs` pattern) and adds [`build_expense_envelope`]: the one-envelope-per-expense move
//! (HEM-13 — no sheet), `source_type = "expense"`, idempotent on accounting's `source_id` dedup key.
//!
//! Posting semantics (Odoo 19 hr_expense, dual payment mode HEB-16/17, tax overlay HEM-6):
//!
//! ```text
//! own_account:     Dr category expense account   amount_total
//!                  Dr tax input accounts         input tax_amounts
//!                  Cr employee payable account   amount + input − withholding
//!                  Cr withholding tax accounts   withholding tax_amounts
//!
//! company_account: same debits · Cr bank account (net) · Cr withholding accounts
//! ```
//!
//! Withholding reduces the reimbursement (Indonesian PPh: withheld from the payee), so it is a
//! CREDIT on the tax account — debits always equal credits and [`AccountingPostEnvelope::
//! is_balanced`] is asserted before the envelope is ever sent. Single currency v1 (IDR, no FX).
//!
//! Zero Cargo edge into backbone-accounting (ADR-0004).

use rust_decimal::Decimal;
use uuid::Uuid;

pub use backbone_gl_posting::{
    AccountingPostEnvelope, GlPostAck, GlPostLine, GlPostRejected, GlPostSink,
};

/// The default sink: nothing is wired (accounting composes in W2). Posting fails loudly with
/// the stable `gl_seam_unwired` code — the row stays `approved` and retryable, never silently
/// unposted, and never marked posted without a GL entry behind it.
pub struct UnwiredGlSink;

#[async_trait::async_trait]
impl GlPostSink for UnwiredGlSink {
    async fn post(
        &self,
        _envelope: &AccountingPostEnvelope,
    ) -> Result<GlPostAck, GlPostRejected> {
        Err(GlPostRejected {
            code: "gl_seam_unwired".to_string(),
            message: "the GL seam is not wired — supply a GlPostSink to post expenses".to_string(),
        })
    }
}

use crate::domain::entity::{Expense, ExpenseCategory, ExpensePaymentMode};

/// The host-resolved accounts the envelope needs but the schema does not own: which payable
/// account the employee draws against and which bank account the company paid from. Accounting
/// (W2) supplies them at composition time — via config or its chart-of-accounts service; until
/// then `post` callers must pass them explicitly (the guarded route takes them on the body).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PostAccounts {
    /// Cr target when `payment_mode = own_account` (the employee's payable).
    pub employee_payable_account_id: Uuid,
    /// Cr target when `payment_mode = company_account` (the cash/bank account).
    pub bank_account_id: Uuid,
}

/// One overlay row as the builder consumes it (basis ∈ input|withholding). Pre-computed by the
/// caller — expenses does no tax computation (billing's removable-overlay pattern).
#[derive(Debug, Clone)]
pub struct TaxLineInput {
    pub basis: String,
    pub account_id: Uuid,
    pub tax_amount: Decimal,
}

/// Why the envelope could not be built. Both are caller-input errors → 422.
#[derive(Debug, thiserror::Error)]
pub enum EnvelopeError {
    #[error("tax line basis must be \"input\" or \"withholding\"")]
    BadBasis,
    #[error("withholding ({0}) exceeds the claim plus input tax ({1})")]
    WithholdingExceedsClaim(Decimal, Decimal),
}

/// Build the balanced GL envelope for one approved expense (HEM-13: one envelope per expense,
/// never per sheet). `posting_date` = the expense date (Odoo semantics); the idempotency key is
/// stable per claim, so a retry after a transport failure reuses accounting's dedup instead of
/// double-posting.
pub fn build_expense_envelope(
    expense: &Expense,
    category: &ExpenseCategory,
    tax_lines: &[TaxLineInput],
    accounts: &PostAccounts,
) -> Result<AccountingPostEnvelope, EnvelopeError> {
    let mut lines = Vec::with_capacity(2 + tax_lines.len());

    // Dr: the category's expense account, gross claim amount, party-tagged to the claimant.
    lines.push(
        GlPostLine::debit(category.expense_account_id, expense.amount_total)
            .with_party("employee", expense.employee_id)
            .with_description(format!(
                "expense {} · {}",
                category.code, expense.description
            )),
    );

    let mut input_tax = Decimal::ZERO;
    let mut withholding = Decimal::ZERO;
    for line in tax_lines {
        if line.tax_amount < Decimal::ZERO {
            return Err(EnvelopeError::BadBasis);
        }
        match line.basis.as_str() {
            "input" => {
                input_tax += line.tax_amount;
                lines.push(GlPostLine::debit(line.account_id, line.tax_amount));
            }
            // Withheld from the payee: a CREDIT on the tax account, reducing the reimbursement.
            "withholding" => {
                withholding += line.tax_amount;
                lines.push(GlPostLine::credit(line.account_id, line.tax_amount));
            }
            _ => return Err(EnvelopeError::BadBasis),
        }
    }

    // Cr: the payable (own_account) or the bank (company_account), net of withholding.
    let credit_account = match expense.payment_mode {
        ExpensePaymentMode::OwnAccount => accounts.employee_payable_account_id,
        ExpensePaymentMode::CompanyAccount => accounts.bank_account_id,
    };
    let gross = expense.amount_total + input_tax;
    if withholding > gross {
        return Err(EnvelopeError::WithholdingExceedsClaim(withholding, gross));
    }
    let net = gross - withholding;
    if net > Decimal::ZERO {
        lines.push(
            GlPostLine::credit(credit_account, net)
                .with_party("employee", expense.employee_id),
        );
    }

    let envelope = AccountingPostEnvelope {
        idempotency_key: format!("expense:{}:{}", expense.company_id, expense.id),
        company_id: expense.company_id,
        branch_id: None,
        source_type: "expense".to_string(),
        source_id: expense.id,
        source_reference: expense.reference.clone(),
        posting_date: expense.expense_date,
        currency: expense.currency.clone(),
        posting_type: "original".to_string(),
        reverses_post_id: None,
        description: Some(format!("expense claim {}", expense.id)),
        lines,
    };
    debug_assert!(
        envelope.is_balanced(),
        "expense envelope must balance: Dr(expense+input) == Cr(net+withholding)"
    );
    Ok(envelope)
}
