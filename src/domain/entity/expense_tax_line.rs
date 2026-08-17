use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;
use rust_decimal::Decimal;
use super::AuditMetadata;

/// Strongly-typed ID for ExpenseTaxLine
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ExpenseTaxLineId(pub Uuid);

impl ExpenseTaxLineId {
    pub fn new(id: Uuid) -> Self { Self(id) }
    pub fn generate() -> Self { Self(Uuid::new_v4()) }
    pub fn into_inner(self) -> Uuid { self.0 }
}

impl std::fmt::Display for ExpenseTaxLineId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::str::FromStr for ExpenseTaxLineId {
    type Err = uuid::Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self(Uuid::parse_str(s)?))
    }
}

impl From<Uuid> for ExpenseTaxLineId {
    fn from(id: Uuid) -> Self { Self(id) }
}

impl From<ExpenseTaxLineId> for Uuid {
    fn from(id: ExpenseTaxLineId) -> Self { id.0 }
}

impl AsRef<Uuid> for ExpenseTaxLineId {
    fn as_ref(&self) -> &Uuid { &self.0 }
}

impl std::ops::Deref for ExpenseTaxLineId {
    type Target = Uuid;
    fn deref(&self) -> &Self::Target { &self.0 }
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct ExpenseTaxLine {
    pub id: Uuid,
    pub company_id: Uuid,
    pub expense_id: Uuid,
    pub basis: String,
    pub account_id: Uuid,
    pub description: Option<String>,
    pub rate: Decimal,
    pub tax_amount: Decimal,
    #[serde(default)]
    #[sqlx(json)]
    pub metadata: AuditMetadata,
}

impl ExpenseTaxLine {
    /// Create a builder for ExpenseTaxLine
    pub fn builder() -> ExpenseTaxLineBuilder {
        <ExpenseTaxLineBuilder as Default>::default()
    }

    /// Create a new ExpenseTaxLine with required fields
    pub fn new(company_id: Uuid, expense_id: Uuid, basis: String, account_id: Uuid, rate: Decimal, tax_amount: Decimal) -> Self {
        Self {
            id: Uuid::new_v4(),
            company_id,
            expense_id,
            basis,
            account_id,
            description: None,
            rate,
            tax_amount,
            metadata: AuditMetadata::default(),
        }
    }

    /// Get the entity's unique identifier
    pub fn id(&self) -> &Uuid {
        &self.id
    }

    /// Get a strongly-typed ID for this entity
    pub fn typed_id(&self) -> ExpenseTaxLineId {
        ExpenseTaxLineId(self.id)
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

    /// Set the description field (chainable)
    pub fn with_description(mut self, value: String) -> Self {
        self.description = Some(value);
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
                "expense_id" => {
                    if let Ok(v) = serde_json::from_value(value) { self.expense_id = v; }
                }
                "basis" => {
                    if let Ok(v) = serde_json::from_value(value) { self.basis = v; }
                }
                "account_id" => {
                    if let Ok(v) = serde_json::from_value(value) { self.account_id = v; }
                }
                "description" => {
                    if let Ok(v) = serde_json::from_value(value) { self.description = v; }
                }
                "rate" => {
                    if let Ok(v) = serde_json::from_value(value) { self.rate = v; }
                }
                "tax_amount" => {
                    if let Ok(v) = serde_json::from_value(value) { self.tax_amount = v; }
                }
                _ => {} // ignore unknown fields
            }
        }
    }

    // <<< CUSTOM METHODS START >>>
    // <<< CUSTOM METHODS END >>>
}

impl super::Entity for ExpenseTaxLine {
    type Id = Uuid;

    fn entity_id(&self) -> &Self::Id {
        &self.id
    }

    fn entity_type() -> &'static str {
        "ExpenseTaxLine"
    }
}

impl backbone_core::PersistentEntity for ExpenseTaxLine {
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

impl backbone_orm::EntityRepoMeta for ExpenseTaxLine {
    fn column_types() -> std::collections::HashMap<String, String> {
        let mut m = std::collections::HashMap::new();
        m.insert("id".to_string(), "uuid".to_string());
        m.insert("company_id".to_string(), "uuid".to_string());
        m.insert("expense_id".to_string(), "uuid".to_string());
        m.insert("account_id".to_string(), "uuid".to_string());
        m
    }
    fn search_fields() -> &'static [&'static str] {
        &["basis"]
    }
    fn company_field() -> Option<&'static str> {
        Some("company_id")
    }
}

/// Builder for ExpenseTaxLine entity
///
/// Provides a fluent API for constructing ExpenseTaxLine instances.
/// System fields (id, metadata, timestamps) are auto-initialized.
#[derive(Debug, Clone, Default)]
pub struct ExpenseTaxLineBuilder {
    company_id: Option<Uuid>,
    expense_id: Option<Uuid>,
    basis: Option<String>,
    account_id: Option<Uuid>,
    description: Option<String>,
    rate: Option<Decimal>,
    tax_amount: Option<Decimal>,
}

impl ExpenseTaxLineBuilder {
    /// Set the company_id field (required)
    pub fn company_id(mut self, value: Uuid) -> Self {
        self.company_id = Some(value);
        self
    }

    /// Set the expense_id field (required)
    pub fn expense_id(mut self, value: Uuid) -> Self {
        self.expense_id = Some(value);
        self
    }

    /// Set the basis field (required)
    pub fn basis(mut self, value: String) -> Self {
        self.basis = Some(value);
        self
    }

    /// Set the account_id field (required)
    pub fn account_id(mut self, value: Uuid) -> Self {
        self.account_id = Some(value);
        self
    }

    /// Set the description field (optional)
    pub fn description(mut self, value: String) -> Self {
        self.description = Some(value);
        self
    }

    /// Set the rate field (required)
    pub fn rate(mut self, value: Decimal) -> Self {
        self.rate = Some(value);
        self
    }

    /// Set the tax_amount field (required)
    pub fn tax_amount(mut self, value: Decimal) -> Self {
        self.tax_amount = Some(value);
        self
    }

    /// Build the ExpenseTaxLine entity
    ///
    /// Returns Err if any required field without a default is missing.
    pub fn build(self) -> Result<ExpenseTaxLine, String> {
        let company_id = self.company_id.ok_or_else(|| "company_id is required".to_string())?;
        let expense_id = self.expense_id.ok_or_else(|| "expense_id is required".to_string())?;
        let basis = self.basis.ok_or_else(|| "basis is required".to_string())?;
        let account_id = self.account_id.ok_or_else(|| "account_id is required".to_string())?;
        let rate = self.rate.ok_or_else(|| "rate is required".to_string())?;
        let tax_amount = self.tax_amount.ok_or_else(|| "tax_amount is required".to_string())?;

        Ok(ExpenseTaxLine {
            id: Uuid::new_v4(),
            company_id,
            expense_id,
            basis,
            account_id,
            description: self.description,
            rate,
            tax_amount,
            metadata: AuditMetadata::default(),
        })
    }
}
