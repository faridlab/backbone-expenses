use chrono::{DateTime, Utc, NaiveDate};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;
use rust_decimal::Decimal;

use super::ExpensePaymentMode;
use super::ExpenseApprovalState;
use super::ExpenseState;
use super::AuditMetadata;

/// Strongly-typed ID for Expense
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ExpenseId(pub Uuid);

impl ExpenseId {
    pub fn new(id: Uuid) -> Self { Self(id) }
    pub fn generate() -> Self { Self(Uuid::new_v4()) }
    pub fn into_inner(self) -> Uuid { self.0 }
}

impl std::fmt::Display for ExpenseId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::str::FromStr for ExpenseId {
    type Err = uuid::Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self(Uuid::parse_str(s)?))
    }
}

impl From<Uuid> for ExpenseId {
    fn from(id: Uuid) -> Self { Self(id) }
}

impl From<ExpenseId> for Uuid {
    fn from(id: ExpenseId) -> Self { id.0 }
}

impl AsRef<Uuid> for ExpenseId {
    fn as_ref(&self) -> &Uuid { &self.0 }
}

impl std::ops::Deref for ExpenseId {
    type Target = Uuid;
    fn deref(&self) -> &Self::Target { &self.0 }
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Expense {
    pub id: Uuid,
    pub company_id: Uuid,
    pub employee_id: Uuid,
    pub category_id: Uuid,
    pub expense_date: NaiveDate,
    pub description: String,
    pub amount_total: Decimal,
    pub currency: String,
    pub payment_mode: ExpensePaymentMode,
    pub reference: Option<String>,
    pub approval_state: ExpenseApprovalState,
    pub state: ExpenseState,
    pub approval_request_id: Option<Uuid>,
    pub journal_id: Option<Uuid>,
    pub accounting_post_id: Option<Uuid>,
    pub reimbursement_id: Option<Uuid>,
    pub receipt_file_id: Option<Uuid>,
    #[serde(default)]
    #[sqlx(json)]
    pub metadata: AuditMetadata,
}

impl Expense {
    /// Create a builder for Expense
    pub fn builder() -> ExpenseBuilder {
        <ExpenseBuilder as Default>::default()
    }

    /// Create a new Expense with required fields
    pub fn new(company_id: Uuid, employee_id: Uuid, category_id: Uuid, expense_date: NaiveDate, description: String, amount_total: Decimal, currency: String, payment_mode: ExpensePaymentMode, approval_state: ExpenseApprovalState, state: ExpenseState) -> Self {
        Self {
            id: Uuid::new_v4(),
            company_id,
            employee_id,
            category_id,
            expense_date,
            description,
            amount_total,
            currency,
            payment_mode,
            reference: None,
            approval_state,
            state,
            approval_request_id: None,
            journal_id: None,
            accounting_post_id: None,
            reimbursement_id: None,
            receipt_file_id: None,
            metadata: AuditMetadata::default(),
        }
    }

    /// Get the entity's unique identifier
    pub fn id(&self) -> &Uuid {
        &self.id
    }

    /// Get a strongly-typed ID for this entity
    pub fn typed_id(&self) -> ExpenseId {
        ExpenseId(self.id)
    }

    /// Get when this entity was created
    pub fn created_at(&self) -> Option<&DateTime<Utc>> {
        self.metadata.created_at.as_ref()
    }

    /// Get when this entity was last updated
    pub fn updated_at(&self) -> Option<&DateTime<Utc>> {
        self.metadata.updated_at.as_ref()
    }

    /// Check if this entity is soft deleted
    pub fn is_deleted(&self) -> bool {
        self.metadata.deleted_at.is_some()
    }

    /// Check if this entity is active (not deleted)
    pub fn is_active(&self) -> bool {
        self.metadata.deleted_at.is_none()
    }

    /// Get when this entity was deleted
    pub fn deleted_at(&self) -> Option<&DateTime<Utc>> {
        self.metadata.deleted_at.as_ref()
    }

    /// Get who created this entity
    pub fn created_by(&self) -> Option<&Uuid> {
        self.metadata.created_by.as_ref()
    }

    /// Get who last updated this entity
    pub fn updated_by(&self) -> Option<&Uuid> {
        self.metadata.updated_by.as_ref()
    }

    /// Get who deleted this entity
    pub fn deleted_by(&self) -> Option<&Uuid> {
        self.metadata.deleted_by.as_ref()
    }


    // ==========================================================
    // Fluent Setters (with_* for optional fields)
    // ==========================================================

    /// Set the reference field (chainable)
    pub fn with_reference(mut self, value: String) -> Self {
        self.reference = Some(value);
        self
    }

    /// Set the approval_request_id field (chainable)
    pub fn with_approval_request_id(mut self, value: Uuid) -> Self {
        self.approval_request_id = Some(value);
        self
    }

    /// Set the journal_id field (chainable)
    pub fn with_journal_id(mut self, value: Uuid) -> Self {
        self.journal_id = Some(value);
        self
    }

    /// Set the accounting_post_id field (chainable)
    pub fn with_accounting_post_id(mut self, value: Uuid) -> Self {
        self.accounting_post_id = Some(value);
        self
    }

    /// Set the reimbursement_id field (chainable)
    pub fn with_reimbursement_id(mut self, value: Uuid) -> Self {
        self.reimbursement_id = Some(value);
        self
    }

    /// Set the receipt_file_id field (chainable)
    pub fn with_receipt_file_id(mut self, value: Uuid) -> Self {
        self.receipt_file_id = Some(value);
        self
    }

    // ==========================================================
    // Partial Update
    // ==========================================================

    /// Apply partial updates from a map of field name to JSON value
    pub fn apply_patch(&mut self, fields: std::collections::HashMap<String, serde_json::Value>) {
        for (key, value) in fields {
            match key.as_str() {
                "company_id" => {
                    if let Ok(v) = serde_json::from_value(value) { self.company_id = v; }
                }
                "employee_id" => {
                    if let Ok(v) = serde_json::from_value(value) { self.employee_id = v; }
                }
                "category_id" => {
                    if let Ok(v) = serde_json::from_value(value) { self.category_id = v; }
                }
                "expense_date" => {
                    if let Ok(v) = serde_json::from_value(value) { self.expense_date = v; }
                }
                "description" => {
                    if let Ok(v) = serde_json::from_value(value) { self.description = v; }
                }
                "amount_total" => {
                    if let Ok(v) = serde_json::from_value(value) { self.amount_total = v; }
                }
                "currency" => {
                    if let Ok(v) = serde_json::from_value(value) { self.currency = v; }
                }
                "payment_mode" => {
                    if let Ok(v) = serde_json::from_value(value) { self.payment_mode = v; }
                }
                "reference" => {
                    if let Ok(v) = serde_json::from_value(value) { self.reference = v; }
                }
                "approval_state" => {
                    if let Ok(v) = serde_json::from_value(value) { self.approval_state = v; }
                }
                "state" => {
                    if let Ok(v) = serde_json::from_value(value) { self.state = v; }
                }
                "approval_request_id" => {
                    if let Ok(v) = serde_json::from_value(value) { self.approval_request_id = v; }
                }
                "journal_id" => {
                    if let Ok(v) = serde_json::from_value(value) { self.journal_id = v; }
                }
                "accounting_post_id" => {
                    if let Ok(v) = serde_json::from_value(value) { self.accounting_post_id = v; }
                }
                "reimbursement_id" => {
                    if let Ok(v) = serde_json::from_value(value) { self.reimbursement_id = v; }
                }
                "receipt_file_id" => {
                    if let Ok(v) = serde_json::from_value(value) { self.receipt_file_id = v; }
                }
                _ => {} // ignore unknown fields
            }
        }
    }

    // <<< CUSTOM METHODS START >>>
    // <<< CUSTOM METHODS END >>>
}

impl super::Entity for Expense {
    type Id = Uuid;

    fn entity_id(&self) -> &Self::Id {
        &self.id
    }

    fn entity_type() -> &'static str {
        "Expense"
    }
}

impl backbone_core::PersistentEntity for Expense {
    fn entity_id(&self) -> String {
        self.id.to_string()
    }
    fn set_entity_id(&mut self, id: String) {
        if let Ok(uuid) = uuid::Uuid::parse_str(&id) {
            self.id = uuid;
        }
    }
    fn created_at(&self) -> Option<chrono::DateTime<chrono::Utc>> {
        self.metadata.created_at
    }
    fn set_created_at(&mut self, ts: chrono::DateTime<chrono::Utc>) {
        self.metadata.created_at = Some(ts);
    }
    fn updated_at(&self) -> Option<chrono::DateTime<chrono::Utc>> {
        self.metadata.updated_at
    }
    fn set_updated_at(&mut self, ts: chrono::DateTime<chrono::Utc>) {
        self.metadata.updated_at = Some(ts);
    }
    fn deleted_at(&self) -> Option<chrono::DateTime<chrono::Utc>> {
        self.metadata.deleted_at
    }
    fn set_deleted_at(&mut self, ts: Option<chrono::DateTime<chrono::Utc>>) {
        self.metadata.deleted_at = ts;
    }
}

impl backbone_orm::EntityRepoMeta for Expense {
    fn column_types() -> std::collections::HashMap<String, String> {
        let mut m = std::collections::HashMap::new();
        m.insert("id".to_string(), "uuid".to_string());
        m.insert("company_id".to_string(), "uuid".to_string());
        m.insert("employee_id".to_string(), "uuid".to_string());
        m.insert("category_id".to_string(), "uuid".to_string());
        m.insert("approval_request_id".to_string(), "uuid".to_string());
        m.insert("journal_id".to_string(), "uuid".to_string());
        m.insert("accounting_post_id".to_string(), "uuid".to_string());
        m.insert("reimbursement_id".to_string(), "uuid".to_string());
        m.insert("receipt_file_id".to_string(), "uuid".to_string());
        m.insert("payment_mode".to_string(), "expense_payment_mode".to_string());
        m.insert("approval_state".to_string(), "expense_approval_state".to_string());
        m.insert("state".to_string(), "expense_state".to_string());
        m
    }
    fn search_fields() -> &'static [&'static str] {
        &["description", "currency"]
    }
    fn company_field() -> Option<&'static str> {
        Some("company_id")
    }
}

/// Builder for Expense entity
///
/// Provides a fluent API for constructing Expense instances.
/// System fields (id, metadata, timestamps) are auto-initialized.
#[derive(Debug, Clone, Default)]
pub struct ExpenseBuilder {
    company_id: Option<Uuid>,
    employee_id: Option<Uuid>,
    category_id: Option<Uuid>,
    expense_date: Option<NaiveDate>,
    description: Option<String>,
    amount_total: Option<Decimal>,
    currency: Option<String>,
    payment_mode: Option<ExpensePaymentMode>,
    reference: Option<String>,
    approval_state: Option<ExpenseApprovalState>,
    state: Option<ExpenseState>,
    approval_request_id: Option<Uuid>,
    journal_id: Option<Uuid>,
    accounting_post_id: Option<Uuid>,
    reimbursement_id: Option<Uuid>,
    receipt_file_id: Option<Uuid>,
}

impl ExpenseBuilder {
    /// Set the company_id field (required)
    pub fn company_id(mut self, value: Uuid) -> Self {
        self.company_id = Some(value);
        self
    }

    /// Set the employee_id field (required)
    pub fn employee_id(mut self, value: Uuid) -> Self {
        self.employee_id = Some(value);
        self
    }

    /// Set the category_id field (required)
    pub fn category_id(mut self, value: Uuid) -> Self {
        self.category_id = Some(value);
        self
    }

    /// Set the expense_date field (required)
    pub fn expense_date(mut self, value: NaiveDate) -> Self {
        self.expense_date = Some(value);
        self
    }

    /// Set the description field (required)
    pub fn description(mut self, value: String) -> Self {
        self.description = Some(value);
        self
    }

    /// Set the amount_total field (required)
    pub fn amount_total(mut self, value: Decimal) -> Self {
        self.amount_total = Some(value);
        self
    }

    /// Set the currency field (default: `"IDR".to_string()`)
    pub fn currency(mut self, value: String) -> Self {
        self.currency = Some(value);
        self
    }

    /// Set the payment_mode field (required)
    pub fn payment_mode(mut self, value: ExpensePaymentMode) -> Self {
        self.payment_mode = Some(value);
        self
    }

    /// Set the reference field (optional)
    pub fn reference(mut self, value: String) -> Self {
        self.reference = Some(value);
        self
    }

    /// Set the approval_state field (required)
    pub fn approval_state(mut self, value: ExpenseApprovalState) -> Self {
        self.approval_state = Some(value);
        self
    }

    /// Set the state field (required)
    pub fn state(mut self, value: ExpenseState) -> Self {
        self.state = Some(value);
        self
    }

    /// Set the approval_request_id field (optional)
    pub fn approval_request_id(mut self, value: Uuid) -> Self {
        self.approval_request_id = Some(value);
        self
    }

    /// Set the journal_id field (optional)
    pub fn journal_id(mut self, value: Uuid) -> Self {
        self.journal_id = Some(value);
        self
    }

    /// Set the accounting_post_id field (optional)
    pub fn accounting_post_id(mut self, value: Uuid) -> Self {
        self.accounting_post_id = Some(value);
        self
    }

    /// Set the reimbursement_id field (optional)
    pub fn reimbursement_id(mut self, value: Uuid) -> Self {
        self.reimbursement_id = Some(value);
        self
    }

    /// Set the receipt_file_id field (optional)
    pub fn receipt_file_id(mut self, value: Uuid) -> Self {
        self.receipt_file_id = Some(value);
        self
    }

    /// Build the Expense entity
    ///
    /// Returns Err if any required field without a default is missing.
    pub fn build(self) -> Result<Expense, String> {
        let company_id = self.company_id.ok_or_else(|| "company_id is required".to_string())?;
        let employee_id = self.employee_id.ok_or_else(|| "employee_id is required".to_string())?;
        let category_id = self.category_id.ok_or_else(|| "category_id is required".to_string())?;
        let expense_date = self.expense_date.ok_or_else(|| "expense_date is required".to_string())?;
        let description = self.description.ok_or_else(|| "description is required".to_string())?;
        let amount_total = self.amount_total.ok_or_else(|| "amount_total is required".to_string())?;
        let payment_mode = self.payment_mode.ok_or_else(|| "payment_mode is required".to_string())?;
        let approval_state = self.approval_state.ok_or_else(|| "approval_state is required".to_string())?;
        let state = self.state.ok_or_else(|| "state is required".to_string())?;

        Ok(Expense {
            id: Uuid::new_v4(),
            company_id,
            employee_id,
            category_id,
            expense_date,
            description,
            amount_total,
            currency: self.currency.unwrap_or("IDR".to_string()),
            payment_mode,
            reference: self.reference,
            approval_state,
            state,
            approval_request_id: self.approval_request_id,
            journal_id: self.journal_id,
            accounting_post_id: self.accounting_post_id,
            reimbursement_id: self.reimbursement_id,
            receipt_file_id: self.receipt_file_id,
            metadata: AuditMetadata::default(),
        })
    }
}
