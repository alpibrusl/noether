mod builder;
mod hash;
pub mod property;
mod schema;
mod signing;
pub mod spec;
pub mod validation;

pub use builder::{StageBuilder, StageBuilderError};
#[allow(deprecated)]
pub use hash::compute_canonical_id;
pub use hash::{canonical_json, compute_signature_id, compute_stage_id};
pub use property::{Property, PropertyViolation};
#[allow(deprecated)]
pub use schema::CanonicalId;
pub use schema::{
    CostEstimate, Example, ImplementationId, SignatureId, Stage, StageId, StageLifecycle,
    StageSignature,
};
pub use signing::{sign_stage_id, verify_stage_signature, SigningError};
pub use spec::{normalize_type, parse_simple_spec};
