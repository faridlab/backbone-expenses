//! The module's extension (hand-written; ADR-0031): what the module adds beside its
//! generated services. The generated module and builder carry `ModuleExt` and
//! `ModuleBuilderExt` and dereference to them, so their fields read as `module.field`.

#[allow(unused_imports)]
use super::*;

/// State the module adds; read through the generated module's `Deref`.
pub struct ModuleExt {
    // The validated claim verbs (create/update/submit/approve/refuse/post/settle, tax overlay,
    // receipts) and their three seam ports.
    pub(crate) expenses_write_service: Arc<ExpensesWriteService>,
}

/// State the builder adds.
pub struct ModuleBuilderExt {
}

impl Default for ModuleBuilderExt {
    fn default() -> Self {
        Self {
        }
    }
}

impl ModuleBuilderExt {
    /// Build the extension's state from what the generated build made.
    #[allow(unused_variables, clippy::redundant_clone)]
    pub(crate) fn build(self, parts: &ModuleParts<'_>) -> anyhow::Result<ModuleExt> {
        let db_pool = parts.db_pool.clone();
        // The validated write path (H-4): verbs + seams, all default-unwired (approve is
        // TR2-fail-closed, post/settle fail closed until W2/P6 wire the adapters).
        let expenses_write_service = Arc::new(ExpensesWriteService::new(db_pool.clone()));
        Ok(ModuleExt {
            expenses_write_service,
        })
    }
}

impl crate::ThisModule {
    /// The validated write path (seams default-unwired). Composition points that need to wire
    /// the approvals/GL/reimbursement ports build their own from the same pool:
    /// `ExpensesWriteService::new(pool).with_gl_sink(sink).with_approvals(port)`.
    pub fn expenses_write_service(&self) -> Arc<ExpensesWriteService> {
        self.expenses_write_service.clone()
    }
    /// Arm the approvals seam on the module-built service after the fact (last writer wins;
    /// callers arm exactly once at composition time).
    pub fn set_expenses_approvals(&self, port: Arc<dyn ApprovalFiling>) {
        self.expenses_write_service.set_approvals(port);
    }
    /// Replace the write service at the composition root — how a host swaps in a service built
    /// with real adapters over the fail-closed seams (GL posting, reimbursement) or its own
    /// approvals port. Consuming `self` keeps the chain reading naturally; call before mounting
    /// routes, the module is consumed once at startup.
    pub fn with_expenses_write_service(mut self, svc: Arc<ExpensesWriteService>) -> Self {
        self.expenses_write_service = svc;
        self
    }
}
