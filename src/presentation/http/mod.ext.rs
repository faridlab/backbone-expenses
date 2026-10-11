// This directory's extension (hand-written; ADR-0031). The generated mod.rs (lib.rs for
// src/) pulls it in with include!, so its lines resolve against this directory.

// The guarded composition (validated claim verbs + report projection + category CRUD +
// safe reads) — the RECOMMENDED mount for production hosts.
pub mod guarded_routes;
pub use guarded_routes::create_guarded_expenses_routes;
