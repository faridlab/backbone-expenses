use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;
use rust_decimal::Decimal;
use super::AuditMetadata;

/// Strongly-typed ID for ExpenseCategory
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ExpenseCategoryId(pub Uuid);

impl ExpenseCategoryId {
    pub fn new(id: Uuid) -> Self { Self(id) }
    pub fn generate() -> Self { Self(Uuid::new_v4()) }
    pub fn into_inner(self) -> Uuid { self.0 }
}

impl std::fmt::Display for ExpenseCategoryId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::str::FromStr for ExpenseCategoryId {
    type Err = uuid::Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self(Uuid::parse_str(s)?))
    }
}

impl From<Uuid> for ExpenseCategoryId {
    fn from(id: Uuid) -> Self { Self(id) }
}

impl From<ExpenseCategoryId> for Uuid {
    fn from(id: ExpenseCategoryId) -> Self { id.0 }
}

impl AsRef<Uuid> for ExpenseCategoryId {
    fn as_ref(&self) -> &Uuid { &self.0 }
}

impl std::ops::Deref for ExpenseCategoryId {
    type Target = Uuid;
    fn deref(&self) -> &Self::Target { &self.0 }
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct ExpenseCategory {
    pub id: Uuid,
    pub company_id: Uuid,
    pub code: String,
    pub name: String,
    pub expense_account_id: Uuid,
    pub default_tax_rate: Option<Decimal>,
    pub description: Option<String>,
    #[serde(default)]
    #[sqlx(json)]
    pub metadata: AuditMetadata,
}

impl ExpenseCategory {
    /// Create a builder for ExpenseCategory
    pub fn builder() -> ExpenseCategoryBuilder {
        <ExpenseCategoryBuilder as Default>::default()
    }

    /// Create a new ExpenseCategory with required fields
    pub fn new(company_id: Uuid, code: String, name: String, expense_account_id: Uuid) -> Self {
        Self {
            id: Uuid::new_v4(),
            company_id,
            code,
            name,
            expense_account_id,
            default_tax_rate: None,
            description: None,
            metadata: AuditMetadata::default(),
        }
    }

    /// Get the entity's unique identifier
    pub fn id(&self) -> &Uuid {
        &self.id
    }

    /// Get a strongly-typed ID for this entity
    pub fn typed_id(&self) -> ExpenseCategoryId {
        ExpenseCategoryId(self.id)
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

    /// Set the default_tax_rate field (chainable)
    pub fn with_default_tax_rate(mut self, value: Decimal) -> Self {
        self.default_tax_rate = Some(value);
        self
    }

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
                "code" => {
                    if let Ok(v) = serde_json::from_value(value) { self.code = v; }
                }
                "name" => {
                    if let Ok(v) = serde_json::from_value(value) { self.name = v; }
                }
                "expense_account_id" => {
                    if let Ok(v) = serde_json::from_value(value) { self.expense_account_id = v; }
                }
                "default_tax_rate" => {
                    if let Ok(v) = serde_json::from_value(value) { self.default_tax_rate = v; }
                }
                "description" => {
                    if let Ok(v) = serde_json::from_value(value) { self.description = v; }
                }
                _ => {} // ignore unknown fields
            }
        }
    }

    // <<< CUSTOM METHODS START >>>
    // <<< CUSTOM METHODS END >>>
}

impl super::Entity for ExpenseCategory {
    type Id = Uuid;

    fn entity_id(&self) -> &Self::Id {
        &self.id
    }

    fn entity_type() -> &'static str {
        "ExpenseCategory"
    }
}

impl backbone_core::PersistentEntity for ExpenseCategory {
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

impl backbone_orm::EntityRepoMeta for ExpenseCategory {
    fn column_types() -> std::collections::HashMap<String, String> {
        let mut m = std::collections::HashMap::new();
        m.insert("id".to_string(), "uuid".to_string());
        m.insert("company_id".to_string(), "uuid".to_string());
        m.insert("expense_account_id".to_string(), "uuid".to_string());
        m
    }
    fn search_fields() -> &'static [&'static str] {
        &["code", "name"]
    }
    fn company_field() -> Option<&'static str> {
        Some("company_id")
    }
}

/// Builder for ExpenseCategory entity
///
/// Provides a fluent API for constructing ExpenseCategory instances.
/// System fields (id, metadata, timestamps) are auto-initialized.
#[derive(Debug, Clone, Default)]
pub struct ExpenseCategoryBuilder {
    company_id: Option<Uuid>,
    code: Option<String>,
    name: Option<String>,
    expense_account_id: Option<Uuid>,
    default_tax_rate: Option<Decimal>,
    description: Option<String>,
}

impl ExpenseCategoryBuilder {
    /// Set the company_id field (required)
    pub fn company_id(mut self, value: Uuid) -> Self {
        self.company_id = Some(value);
        self
    }

    /// Set the code field (required)
    pub fn code(mut self, value: String) -> Self {
        self.code = Some(value);
        self
    }

    /// Set the name field (required)
    pub fn name(mut self, value: String) -> Self {
        self.name = Some(value);
        self
    }

    /// Set the expense_account_id field (required)
    pub fn expense_account_id(mut self, value: Uuid) -> Self {
        self.expense_account_id = Some(value);
        self
    }

    /// Set the default_tax_rate field (optional)
    pub fn default_tax_rate(mut self, value: Decimal) -> Self {
        self.default_tax_rate = Some(value);
        self
    }

    /// Set the description field (optional)
    pub fn description(mut self, value: String) -> Self {
        self.description = Some(value);
        self
    }

    /// Build the ExpenseCategory entity
    ///
    /// Returns Err if any required field without a default is missing.
    pub fn build(self) -> Result<ExpenseCategory, String> {
        let company_id = self.company_id.ok_or_else(|| "company_id is required".to_string())?;
        let code = self.code.ok_or_else(|| "code is required".to_string())?;
        let name = self.name.ok_or_else(|| "name is required".to_string())?;
        let expense_account_id = self.expense_account_id.ok_or_else(|| "expense_account_id is required".to_string())?;

        Ok(ExpenseCategory {
            id: Uuid::new_v4(),
            company_id,
            code,
            name,
            expense_account_id,
            default_tax_rate: self.default_tax_rate,
            description: self.description,
            metadata: AuditMetadata::default(),
        })
    }
}
