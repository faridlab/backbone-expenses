use serde::{Deserialize, Serialize};
use sqlx::Type;
use std::str::FromStr;
#[cfg(feature = "openapi")]
use utoipa::ToSchema;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Type)]
#[cfg_attr(feature = "openapi", derive(ToSchema))]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "expense_state", rename_all = "snake_case")]
pub enum ExpenseState {
    Draft,
    Submitted,
    Approved,
    Posted,
    Done,
    Refused,
}

impl std::fmt::Display for ExpenseState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Draft => write!(f, "draft"),
            Self::Submitted => write!(f, "submitted"),
            Self::Approved => write!(f, "approved"),
            Self::Posted => write!(f, "posted"),
            Self::Done => write!(f, "done"),
            Self::Refused => write!(f, "refused"),
        }
    }
}

impl FromStr for ExpenseState {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "draft" => Ok(Self::Draft),
            "submitted" => Ok(Self::Submitted),
            "approved" => Ok(Self::Approved),
            "posted" => Ok(Self::Posted),
            "done" => Ok(Self::Done),
            "refused" => Ok(Self::Refused),
            _ => Err(format!("Unknown ExpenseState variant: {}", s)),
        }
    }
}

impl Default for ExpenseState {
    fn default() -> Self {
        Self::Draft
    }
}
