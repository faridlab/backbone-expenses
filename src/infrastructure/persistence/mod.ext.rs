// This directory's extension (hand-written; ADR-0031). The generated mod.rs (lib.rs for
// src/) pulls it in with include!, so its lines resolve against this directory.

// Hand-written repository: every SQL statement of the validated write path (H-4).
pub mod expenses_write_repository;
pub use expenses_write_repository::{
    ExpensePatch, ExpenseReportRow, ExpensesWriteRepository, TaxLineWrite,
};
