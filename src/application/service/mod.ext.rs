// This directory's extension (hand-written; ADR-0031). The generated mod.rs (lib.rs for
// src/) pulls it in with include!, so its lines resolve against this directory.

// The validated write path (H-4) + its three seams: approvals (P6 engine), GL posting
// (shared backbone-gl-posting contract; sink wired by the app over accounting, W2), and
// reimbursement (adapter over payment's create_payment, W2). All default-unwired, fail-closed.
pub mod approvals_port;
pub mod expense_gl;
pub mod expenses_write_service;
pub mod reimbursement_port;
pub use approvals_port::{
    ApprovalFiling, ApprovalSeamError, ApprovalVerdict, ExpenseApprovalFilingRequest,
    UnwiredApprovals,
};
pub use expense_gl::{
    build_expense_envelope, PostAccounts, TaxLineInput, UnwiredGlSink, EnvelopeError,
    AccountingPostEnvelope, GlPostAck, GlPostLine, GlPostRejected, GlPostSink,
};
pub use expenses_write_service::{ExpenseWriteError, ExpensesWriteService, NewExpense};
pub use reimbursement_port::{
    ReimbursementAck, ReimbursementRequest, ReimbursementSeamError, ReimbursementSink,
    UnwiredReimbursement,
};
