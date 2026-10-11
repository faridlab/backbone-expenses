// This directory's extension (hand-written; ADR-0031). The generated mod.rs (lib.rs for
// src/) pulls it in with include!, so its lines resolve against this directory.

pub mod request_pool;
// The validated write path (H-4), its seam ports (approvals / GL / reimbursement — all
// default-unwired, fail-closed), the guarded HTTP composition, and `org_scope` re-exported
// for the tenancy probe suite (tests see only the public surface — party/attendance convention).
pub use application::service::{
    build_expense_envelope, AccountingPostEnvelope, ApprovalFiling, ApprovalSeamError,
    ApprovalVerdict, EnvelopeError, ExpenseApprovalFilingRequest, ExpenseWriteError,
    ExpensesWriteService, GlPostAck, GlPostLine, GlPostRejected, GlPostSink, NewExpense,
    PostAccounts, ReimbursementAck, ReimbursementRequest, ReimbursementSeamError,
    ReimbursementSink, TaxLineInput, UnwiredApprovals, UnwiredGlSink, UnwiredReimbursement,
};
pub use presentation::http::create_guarded_expenses_routes;
pub use backbone_orm::org_scope;
