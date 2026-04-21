mod checker;
mod display;
mod primitive;
pub mod unification;

pub use checker::{is_subtype_of, IncompatibilityReason, TypeCompatibility};
pub use primitive::NType;
pub use unification::{unify, Substitution, Ty, UnificationError};
