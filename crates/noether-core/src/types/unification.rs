//! Robinson-style first-order unification.
//!
//! Foundation for parametric polymorphism on stage signatures (M3
//! type-system track, see `docs/roadmap.md`). This module ships the
//! **algorithm** — a minimal type representation with unification,
//! substitution, and occurs-check — deliberately decoupled from
//! [`NType`](super::NType).
//!
//! # Scope of this module
//!
//! The algorithm operates on [`Ty`], a small independent type
//! representation with just the shapes unification needs to walk:
//!
//! - [`Ty::Var`] — a type variable (`T`, `U`, …).
//! - [`Ty::Con`] — an atomic type constant (`Text`, `Number`, …).
//! - [`Ty::App`] — a type constructor applied to arguments
//!   (`List<T>`, `Stream<T>`, `Map<K, V>`, …).
//! - [`Ty::Record`] — structural record types, unified field-by-field.
//!
//! # Why a separate representation
//!
//! [`NType`](super::NType) already has ten variants and is the
//! content-hashed surface — adding a `Var` variant to it is a
//! separate, larger change that touches every exhaustive-match site
//! in the engine, planner, checker, and stdlib. Shipping the
//! algorithm first, on its own closed representation, gives us a
//! tested foundation that the NType integration PR can layer onto
//! without mixing concerns.
//!
//! # Unification rules
//!
//! 1. **Var-Any**: [`Ty::Var(x)`] ~ `t` → substitute `x → t`,
//!    provided `x` does not occur free in `t` (occurs check).
//!    Symmetric: `t ~ Ty::Var(x)` is treated the same.
//! 2. **Var-Var**: `Ty::Var(x) ~ Ty::Var(x)` → identity substitution.
//! 3. **Con-Con**: `Ty::Con(a) ~ Ty::Con(b)` → success if `a == b`,
//!    [`UnificationError::Mismatch`] otherwise.
//! 4. **App-App**: `Ty::App(c1, args1) ~ Ty::App(c2, args2)` →
//!    `c1 == c2` and `args1.len() == args2.len()`, then unify
//!    pairwise.
//! 5. **Record-Record**: exact key-set match, then unify each value.
//!    Width-subtype unification (the asymmetric case) is
//!    deliberately out of scope here; the NType integration layer
//!    can handle it explicitly at graph-edge type-check time.
//! 6. Any other pair → [`UnificationError::Mismatch`].
//!
//! The algorithm is invoked on one pair and returns a most general
//! unifier (MGU) as a [`Substitution`]. For multiple independent
//! pairs, apply each resulting substitution to the remaining pairs
//! before unifying them (standard Robinson-style iteration); this
//! module exposes [`Substitution::compose`] for that pattern.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// Minimal type representation for unification.
///
/// Separate from [`NType`](super::NType) on purpose — see the
/// module rustdoc. A follow-up PR adds the conversion layer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Ty {
    /// Type variable bound during unification.
    Var(String),
    /// Atomic type constant (e.g. `"Text"`, `"Number"`, `"Bool"`).
    Con(String),
    /// Type constructor applied to arguments
    /// (e.g. `App("List", [Var("T")])` for `List<T>`).
    App(String, Vec<Ty>),
    /// Structural record. Keys are field names; values are the
    /// field types. `BTreeMap` keeps the ordering deterministic.
    Record(BTreeMap<String, Ty>),
}

/// A substitution — a map from type-variable name to the type it
/// was bound to during unification.
///
/// Invariant: no binding's RHS contains any variable that is also a
/// key of this substitution (this is what makes the substitution
/// **idempotent**: `apply(s, apply(s, t)) == apply(s, t)`).
/// [`Substitution::compose`] preserves the invariant.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Substitution {
    bindings: BTreeMap<String, Ty>,
}

/// Unification failures.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum UnificationError {
    /// A type variable occurs on both sides of a binding attempt,
    /// which would create an infinite type.
    #[error("occurs check failed: variable '{var}' occurs in {ty:?}")]
    OccursCheck { var: String, ty: Ty },

    /// Two non-variable types failed to unify.
    #[error("cannot unify {lhs:?} with {rhs:?}: {reason}")]
    Mismatch {
        lhs: Ty,
        rhs: Ty,
        reason: &'static str,
    },
}

impl Substitution {
    /// An empty substitution — the identity, no bindings.
    pub fn empty() -> Self {
        Self {
            bindings: BTreeMap::new(),
        }
    }

    /// A single-binding substitution: `var → ty`.
    pub fn singleton(var: impl Into<String>, ty: Ty) -> Self {
        let mut bindings = BTreeMap::new();
        bindings.insert(var.into(), ty);
        Self { bindings }
    }

    /// Apply this substitution to a type, returning a new type with
    /// every free occurrence of a bound variable replaced by the
    /// corresponding RHS.
    pub fn apply(&self, ty: &Ty) -> Ty {
        match ty {
            Ty::Var(v) => self.bindings.get(v).cloned().unwrap_or_else(|| ty.clone()),
            Ty::Con(_) => ty.clone(),
            Ty::App(c, args) => Ty::App(c.clone(), args.iter().map(|a| self.apply(a)).collect()),
            Ty::Record(fields) => Ty::Record(
                fields
                    .iter()
                    .map(|(k, v)| (k.clone(), self.apply(v)))
                    .collect(),
            ),
        }
    }

    /// Compose `self` with `other` — produce a substitution
    /// equivalent to first applying `self` then `other`.
    ///
    /// `(other ∘ self)(t) == other.apply(self.apply(t))`.
    ///
    /// The result's bindings are:
    /// - every binding of `self`, with `other` applied to its RHS;
    /// - every binding of `other` whose LHS isn't already bound by
    ///   the above step.
    pub fn compose(&self, other: &Self) -> Self {
        let mut result = BTreeMap::new();
        for (var, ty) in &self.bindings {
            result.insert(var.clone(), other.apply(ty));
        }
        for (var, ty) in &other.bindings {
            result.entry(var.clone()).or_insert_with(|| ty.clone());
        }
        Self { bindings: result }
    }

    /// Look up a binding.
    pub fn get(&self, var: &str) -> Option<&Ty> {
        self.bindings.get(var)
    }

    /// Iterate bindings in deterministic (BTreeMap) order.
    pub fn iter(&self) -> impl Iterator<Item = (&String, &Ty)> {
        self.bindings.iter()
    }

    /// Number of bindings.
    pub fn len(&self) -> usize {
        self.bindings.len()
    }

    /// `true` when the substitution has no bindings.
    pub fn is_empty(&self) -> bool {
        self.bindings.is_empty()
    }
}

/// Return the set of free-variable names in `ty`.
fn free_vars(ty: &Ty) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    collect_free_vars(ty, &mut out);
    out
}

fn collect_free_vars(ty: &Ty, out: &mut BTreeSet<String>) {
    match ty {
        Ty::Var(v) => {
            out.insert(v.clone());
        }
        Ty::Con(_) => {}
        Ty::App(_, args) => {
            for a in args {
                collect_free_vars(a, out);
            }
        }
        Ty::Record(fields) => {
            for v in fields.values() {
                collect_free_vars(v, out);
            }
        }
    }
}

/// Unify two types, producing a most general unifier.
///
/// Robinson-style: walk both types in parallel; each variable
/// encounter adds a binding; an occurs check prevents infinite
/// types.
pub fn unify(lhs: &Ty, rhs: &Ty) -> Result<Substitution, UnificationError> {
    match (lhs, rhs) {
        // Var-Var: same name → identity. Different names → bind lhs to rhs.
        (Ty::Var(a), Ty::Var(b)) if a == b => Ok(Substitution::empty()),
        (Ty::Var(v), t) | (t, Ty::Var(v)) => bind_var(v, t),

        // Con-Con: exact name match.
        (Ty::Con(a), Ty::Con(b)) => {
            if a == b {
                Ok(Substitution::empty())
            } else {
                Err(UnificationError::Mismatch {
                    lhs: lhs.clone(),
                    rhs: rhs.clone(),
                    reason: "different type constants",
                })
            }
        }

        // App-App: constructor name + arity must match; unify pairwise.
        (Ty::App(c1, a1), Ty::App(c2, a2)) => {
            if c1 != c2 {
                return Err(UnificationError::Mismatch {
                    lhs: lhs.clone(),
                    rhs: rhs.clone(),
                    reason: "different type constructors",
                });
            }
            if a1.len() != a2.len() {
                return Err(UnificationError::Mismatch {
                    lhs: lhs.clone(),
                    rhs: rhs.clone(),
                    reason: "type constructor arity mismatch",
                });
            }
            unify_pairwise(a1, a2)
        }

        // Record-Record: exact key set; unify values.
        (Ty::Record(r1), Ty::Record(r2)) => {
            let keys1: BTreeSet<&String> = r1.keys().collect();
            let keys2: BTreeSet<&String> = r2.keys().collect();
            if keys1 != keys2 {
                return Err(UnificationError::Mismatch {
                    lhs: lhs.clone(),
                    rhs: rhs.clone(),
                    reason: "record field sets differ",
                });
            }
            // Unify each field's value in deterministic key order.
            let pairs: Vec<(&Ty, &Ty)> = r1
                .iter()
                .map(|(k, v1)| (v1, r2.get(k).expect("keys match by construction")))
                .collect();
            let (lhs_tys, rhs_tys): (Vec<&Ty>, Vec<&Ty>) = pairs.into_iter().unzip();
            unify_ref_slices(&lhs_tys, &rhs_tys)
        }

        // Any other pair — incompatible shapes.
        _ => Err(UnificationError::Mismatch {
            lhs: lhs.clone(),
            rhs: rhs.clone(),
            reason: "incompatible type shapes",
        }),
    }
}

/// Bind `var` to `ty`, enforcing the occurs check.
fn bind_var(var: &str, ty: &Ty) -> Result<Substitution, UnificationError> {
    if let Ty::Var(v) = ty {
        if v == var {
            return Ok(Substitution::empty());
        }
    }
    if free_vars(ty).contains(var) {
        return Err(UnificationError::OccursCheck {
            var: var.to_string(),
            ty: ty.clone(),
        });
    }
    Ok(Substitution::singleton(var, ty.clone()))
}

/// Unify two slices of types pairwise. Standard Robinson iteration:
/// unify the first pair, apply the resulting substitution to all
/// subsequent pairs, repeat; compose substitutions along the way.
fn unify_pairwise(lhs: &[Ty], rhs: &[Ty]) -> Result<Substitution, UnificationError> {
    let mut subst = Substitution::empty();
    for (a, b) in lhs.iter().zip(rhs.iter()) {
        let a_subst = subst.apply(a);
        let b_subst = subst.apply(b);
        let step = unify(&a_subst, &b_subst)?;
        subst = subst.compose(&step);
    }
    Ok(subst)
}

/// Same as [`unify_pairwise`] but takes slices of references to
/// sidestep an unnecessary clone when called with reference
/// collections.
fn unify_ref_slices(lhs: &[&Ty], rhs: &[&Ty]) -> Result<Substitution, UnificationError> {
    let mut subst = Substitution::empty();
    for (a, b) in lhs.iter().zip(rhs.iter()) {
        let a_subst = subst.apply(a);
        let b_subst = subst.apply(b);
        let step = unify(&a_subst, &b_subst)?;
        subst = subst.compose(&step);
    }
    Ok(subst)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn var(s: &str) -> Ty {
        Ty::Var(s.into())
    }
    fn con(s: &str) -> Ty {
        Ty::Con(s.into())
    }
    fn list(inner: Ty) -> Ty {
        Ty::App("List".into(), vec![inner])
    }
    fn record(fields: &[(&str, Ty)]) -> Ty {
        Ty::Record(
            fields
                .iter()
                .cloned()
                .map(|(k, v)| (k.to_string(), v))
                .collect(),
        )
    }

    // ── Substitution basics ────────────────────────────────────────

    #[test]
    fn apply_identity_leaves_var_alone() {
        let s = Substitution::empty();
        assert_eq!(s.apply(&var("T")), var("T"));
    }

    #[test]
    fn apply_singleton_substitutes_matching_var() {
        let s = Substitution::singleton("T", con("Number"));
        assert_eq!(s.apply(&var("T")), con("Number"));
    }

    #[test]
    fn apply_recurses_into_app() {
        let s = Substitution::singleton("T", con("Number"));
        assert_eq!(s.apply(&list(var("T"))), list(con("Number")));
    }

    #[test]
    fn apply_recurses_into_record() {
        let s = Substitution::singleton("T", con("Text"));
        assert_eq!(
            s.apply(&record(&[("name", var("T")), ("age", con("Number"))])),
            record(&[("name", con("Text")), ("age", con("Number"))])
        );
    }

    #[test]
    fn compose_applies_left_then_right() {
        // self: T → U ; other: U → Number
        // composed: T → Number, U → Number
        let left = Substitution::singleton("T", var("U"));
        let right = Substitution::singleton("U", con("Number"));
        let composed = left.compose(&right);
        assert_eq!(composed.get("T"), Some(&con("Number")));
        assert_eq!(composed.get("U"), Some(&con("Number")));
    }

    #[test]
    fn compose_preserves_idempotence() {
        // After composition, applying the composed substitution
        // twice must equal applying it once.
        let left = Substitution::singleton("T", var("U"));
        let right = Substitution::singleton("U", list(con("Text")));
        let composed = left.compose(&right);
        let t = var("T");
        let once = composed.apply(&t);
        let twice = composed.apply(&once);
        assert_eq!(once, twice);
    }

    // ── Unification rules ──────────────────────────────────────────

    #[test]
    fn unify_same_var_is_identity() {
        let s = unify(&var("T"), &var("T")).unwrap();
        assert!(s.is_empty());
    }

    #[test]
    fn unify_var_with_concrete_type_binds_var() {
        let s = unify(&var("T"), &con("Number")).unwrap();
        assert_eq!(s.get("T"), Some(&con("Number")));
    }

    #[test]
    fn unify_concrete_with_var_binds_var() {
        // Symmetry — var can be on either side.
        let s = unify(&con("Number"), &var("T")).unwrap();
        assert_eq!(s.get("T"), Some(&con("Number")));
    }

    #[test]
    fn unify_equal_constants_is_identity() {
        let s = unify(&con("Number"), &con("Number")).unwrap();
        assert!(s.is_empty());
    }

    #[test]
    fn unify_different_constants_is_mismatch() {
        let err = unify(&con("Number"), &con("Text")).unwrap_err();
        assert!(matches!(err, UnificationError::Mismatch { .. }));
    }

    #[test]
    fn unify_app_with_same_constructor_unifies_args() {
        // List<T> ~ List<Number> → T → Number
        let s = unify(&list(var("T")), &list(con("Number"))).unwrap();
        assert_eq!(s.get("T"), Some(&con("Number")));
    }

    #[test]
    fn unify_app_different_constructors_is_mismatch() {
        let err = unify(
            &Ty::App("List".into(), vec![var("T")]),
            &Ty::App("Stream".into(), vec![var("T")]),
        )
        .unwrap_err();
        assert!(matches!(err, UnificationError::Mismatch { .. }));
    }

    #[test]
    fn unify_app_arity_mismatch_is_mismatch() {
        let err = unify(
            &Ty::App("Map".into(), vec![var("K"), var("V")]),
            &Ty::App("Map".into(), vec![var("K")]),
        )
        .unwrap_err();
        assert!(
            matches!(err, UnificationError::Mismatch { reason, .. } if reason == "type constructor arity mismatch")
        );
    }

    #[test]
    fn unify_records_exact_match_unifies_fields() {
        // { a: T, b: Number } ~ { a: Text, b: U } → T → Text, U → Number
        let r1 = record(&[("a", var("T")), ("b", con("Number"))]);
        let r2 = record(&[("a", con("Text")), ("b", var("U"))]);
        let s = unify(&r1, &r2).unwrap();
        assert_eq!(s.get("T"), Some(&con("Text")));
        assert_eq!(s.get("U"), Some(&con("Number")));
    }

    #[test]
    fn unify_records_with_different_field_sets_is_mismatch() {
        let r1 = record(&[("a", con("Text"))]);
        let r2 = record(&[("b", con("Text"))]);
        let err = unify(&r1, &r2).unwrap_err();
        assert!(
            matches!(err, UnificationError::Mismatch { reason, .. } if reason == "record field sets differ")
        );
    }

    // ── Occurs check ───────────────────────────────────────────────

    #[test]
    fn occurs_check_fires_on_var_inside_app() {
        // T ~ List<T> — binding T → List<T> would make T infinite.
        let err = unify(&var("T"), &list(var("T"))).unwrap_err();
        assert!(matches!(err, UnificationError::OccursCheck { var, .. } if var == "T"));
    }

    #[test]
    fn occurs_check_fires_on_var_inside_record() {
        // T ~ { foo: T }
        let err = unify(&var("T"), &record(&[("foo", var("T"))])).unwrap_err();
        assert!(matches!(err, UnificationError::OccursCheck { .. }));
    }

    #[test]
    fn occurs_check_does_not_fire_on_var_itself() {
        // T ~ T must NOT trigger occurs check — it's the identity.
        assert!(unify(&var("T"), &var("T")).is_ok());
    }

    // ── Transitive unification ─────────────────────────────────────

    #[test]
    fn transitive_unification_through_pairwise_propagates_bindings() {
        // (T, List<U>) ~ (List<Number>, List<Number>)
        // First pair: T ~ List<Number> → T → List<Number>
        // Apply substitution: List<U> ~ List<Number> → U → Number
        // Final subst: { T → List<Number>, U → Number }
        let lhs = vec![var("T"), list(var("U"))];
        let rhs = vec![list(con("Number")), list(con("Number"))];
        let s = unify_pairwise(&lhs, &rhs).unwrap();
        assert_eq!(s.get("T"), Some(&list(con("Number"))));
        assert_eq!(s.get("U"), Some(&con("Number")));
    }

    #[test]
    fn unifier_is_most_general() {
        // T ~ U should give a substitution that leaves both T and U
        // bindable later (MGU property). A naive impl might bind
        // both to some fresh concrete, which is over-specific.
        let s = unify(&var("T"), &var("U")).unwrap();
        // Exactly one of {T, U} should be bound to the other.
        assert_eq!(s.len(), 1);
        // Applying s to T must produce either T or U; similarly U.
        let t_img = s.apply(&var("T"));
        let u_img = s.apply(&var("U"));
        assert_eq!(
            t_img, u_img,
            "T and U must unify to the same type under MGU"
        );
    }

    // ── Serde round-trip ───────────────────────────────────────────

    #[test]
    fn ty_round_trips_through_json() {
        // Wire-format stability: the future NType integration layer
        // may serialise a unification step to disk or across a
        // process boundary.
        let t = Ty::App("Map".into(), vec![con("Text"), list(var("V"))]);
        let json = serde_json::to_string(&t).unwrap();
        let back: Ty = serde_json::from_str(&json).unwrap();
        assert_eq!(t, back);
    }
}
