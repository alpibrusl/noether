mod checker;
mod display;
mod primitive;
pub mod unification;

pub use checker::{is_subtype_of, IncompatibilityReason, TypeCompatibility};
pub use primitive::NType;
pub use unification::{
    ntype_to_ty, ntype_to_ty_with_counter, try_ty_to_ntype, unify, Substitution, Ty,
    UnificationError,
};
