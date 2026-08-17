use serde::{Deserialize, Serialize};
use sqlx::Type;
use std::str::FromStr;
#[cfg(feature = "openapi")]
use utoipa::ToSchema;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Type)]
#[cfg_attr(feature = "openapi", derive(ToSchema))]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "expense_payment_mode", rename_all = "snake_case")]
pub enum ExpensePaymentMode {
    OwnAccount,
    CompanyAccount,
}

impl std::fmt::Display for ExpensePaymentMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::OwnAccount => write!(f, "own_account"),
            Self::CompanyAccount => write!(f, "company_account"),
        }
    }
}

impl FromStr for ExpensePaymentMode {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "own_account" => Ok(Self::OwnAccount),
            "company_account" => Ok(Self::CompanyAccount),
            _ => Err(format!("Unknown ExpensePaymentMode variant: {}", s)),
        }
    }
}

impl Default for ExpensePaymentMode {
    fn default() -> Self {
        Self::OwnAccount
    }
}
