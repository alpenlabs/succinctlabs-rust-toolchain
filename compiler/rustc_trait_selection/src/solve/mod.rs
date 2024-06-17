//! The next-generation trait solver, currently still WIP.
//!
//! As a user of rust, you can use `-Znext-solver` to enable the new trait solver.
//!
//! As a developer of rustc, you shouldn't be using the new trait
//! solver without asking the trait-system-refactor-initiative, but it can
//! be enabled with `InferCtxtBuilder::with_next_trait_solver`. This will
//! ensure that trait solving using that inference context will be routed
//! to the new trait solver.
//!
//! For a high-level overview of how this solver works, check out the relevant
//! section of the rustc-dev-guide.
//!
//! FIXME(@lcnr): Write that section. If you read this before then ask me
//! about it on zulip.

use self::infcx::SolverDelegate;
use rustc_hir::def_id::DefId;
use rustc_infer::infer::canonical::Canonical;
use rustc_infer::traits::query::NoSolution;
use rustc_macros::extension;
use rustc_middle::bug;
use rustc_middle::traits::solve::{
    CanonicalResponse, Certainty, ExternalConstraintsData, Goal, GoalSource, QueryResult, Response,
};
use rustc_middle::ty::{
    self, AliasRelationDirection, CoercePredicate, RegionOutlivesPredicate, SubtypePredicate, Ty,
    TyCtxt, TypeOutlivesPredicate, UniverseIndex,
};
use rustc_type_ir::solve::SolverMode;
use rustc_type_ir::{self as ir, Interner};

mod alias_relate;
mod assembly;
mod eval_ctxt;
mod fulfill;
mod infcx;
pub mod inspect;
mod normalize;
mod normalizes_to;
mod project_goals;
mod search_graph;
mod select;
mod trait_goals;

pub use eval_ctxt::{EvalCtxt, GenerateProofTree, InferCtxtEvalExt};
pub use fulfill::{FulfillmentCtxt, NextSolverError};
pub(crate) use normalize::deeply_normalize_for_diagnostics;
pub use normalize::{deeply_normalize, deeply_normalize_with_skipped_universes};
pub use select::InferCtxtSelectExt;

/// How many fixpoint iterations we should attempt inside of the solver before bailing
/// with overflow.
///
/// We previously used  `tcx.recursion_limit().0.checked_ilog2().unwrap_or(0)` for this.
/// However, it feels unlikely that uncreasing the recursion limit by a power of two
/// to get one more itereation is every useful or desirable. We now instead used a constant
/// here. If there ever ends up some use-cases where a bigger number of fixpoint iterations
/// is required, we can add a new attribute for that or revert this to be dependant on the
/// recursion limit again. However, this feels very unlikely.
const FIXPOINT_STEP_LIMIT: usize = 8;

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
enum GoalEvaluationKind {
    Root,
    Nested,
}

#[extension(trait CanonicalResponseExt)]
impl<'tcx> Canonical<'tcx, Response<TyCtxt<'tcx>>> {
    fn has_no_inference_or_external_constraints(&self) -> bool {
        self.value.external_constraints.region_constraints.is_empty()
            && self.value.var_values.is_identity()
            && self.value.external_constraints.opaque_types.is_empty()
    }
}

impl<'a, 'tcx> EvalCtxt<'a, SolverDelegate<'tcx>> {
    #[instrument(level = "trace", skip(self))]
    fn compute_type_outlives_goal(
        &mut self,
        goal: Goal<'tcx, TypeOutlivesPredicate<'tcx>>,
    ) -> QueryResult<'tcx> {
        let ty::OutlivesPredicate(ty, lt) = goal.predicate;
        self.register_ty_outlives(ty, lt);
        self.evaluate_added_goals_and_make_canonical_response(Certainty::Yes)
    }

    #[instrument(level = "trace", skip(self))]
    fn compute_region_outlives_goal(
        &mut self,
        goal: Goal<'tcx, RegionOutlivesPredicate<'tcx>>,
    ) -> QueryResult<'tcx> {
        let ty::OutlivesPredicate(a, b) = goal.predicate;
        self.register_region_outlives(a, b);
        self.evaluate_added_goals_and_make_canonical_response(Certainty::Yes)
    }

    #[instrument(level = "trace", skip(self))]
    fn compute_coerce_goal(
        &mut self,
        goal: Goal<'tcx, CoercePredicate<'tcx>>,
    ) -> QueryResult<'tcx> {
        self.compute_subtype_goal(Goal {
            param_env: goal.param_env,
            predicate: SubtypePredicate {
                a_is_expected: false,
                a: goal.predicate.a,
                b: goal.predicate.b,
            },
        })
    }

    #[instrument(level = "trace", skip(self))]
    fn compute_subtype_goal(
        &mut self,
        goal: Goal<'tcx, SubtypePredicate<'tcx>>,
    ) -> QueryResult<'tcx> {
        if goal.predicate.a.is_ty_var() && goal.predicate.b.is_ty_var() {
            self.evaluate_added_goals_and_make_canonical_response(Certainty::AMBIGUOUS)
        } else {
            self.sub(goal.param_env, goal.predicate.a, goal.predicate.b)?;
            self.evaluate_added_goals_and_make_canonical_response(Certainty::Yes)
        }
    }

    fn compute_object_safe_goal(&mut self, trait_def_id: DefId) -> QueryResult<'tcx> {
        if self.interner().is_object_safe(trait_def_id) {
            self.evaluate_added_goals_and_make_canonical_response(Certainty::Yes)
        } else {
            Err(NoSolution)
        }
    }

    #[instrument(level = "trace", skip(self))]
    fn compute_well_formed_goal(
        &mut self,
        goal: Goal<'tcx, ty::GenericArg<'tcx>>,
    ) -> QueryResult<'tcx> {
        match self.well_formed_goals(goal.param_env, goal.predicate) {
            Some(goals) => {
                self.add_goals(GoalSource::Misc, goals);
                self.evaluate_added_goals_and_make_canonical_response(Certainty::Yes)
            }
            None => self.evaluate_added_goals_and_make_canonical_response(Certainty::AMBIGUOUS),
        }
    }

    #[instrument(level = "trace", skip(self))]
    fn compute_const_evaluatable_goal(
        &mut self,
        Goal { param_env, predicate: ct }: Goal<'tcx, ty::Const<'tcx>>,
    ) -> QueryResult<'tcx> {
        match ct.kind() {
            ty::ConstKind::Unevaluated(uv) => {
                // We never return `NoSolution` here as `try_const_eval_resolve` emits an
                // error itself when failing to evaluate, so emitting an additional fulfillment
                // error in that case is unnecessary noise. This may change in the future once
                // evaluation failures are allowed to impact selection, e.g. generic const
                // expressions in impl headers or `where`-clauses.

                // FIXME(generic_const_exprs): Implement handling for generic
                // const expressions here.
                if let Some(_normalized) = self.try_const_eval_resolve(param_env, uv) {
                    self.evaluate_added_goals_and_make_canonical_response(Certainty::Yes)
                } else {
                    self.evaluate_added_goals_and_make_canonical_response(Certainty::AMBIGUOUS)
                }
            }
            ty::ConstKind::Infer(_) => {
                self.evaluate_added_goals_and_make_canonical_response(Certainty::AMBIGUOUS)
            }
            ty::ConstKind::Placeholder(_)
            | ty::ConstKind::Value(_, _)
            | ty::ConstKind::Error(_) => {
                self.evaluate_added_goals_and_make_canonical_response(Certainty::Yes)
            }
            // We can freely ICE here as:
            // - `Param` gets replaced with a placeholder during canonicalization
            // - `Bound` cannot exist as we don't have a binder around the self Type
            // - `Expr` is part of `feature(generic_const_exprs)` and is not implemented yet
            ty::ConstKind::Param(_) | ty::ConstKind::Bound(_, _) | ty::ConstKind::Expr(_) => {
                bug!("unexpect const kind: {:?}", ct)
            }
        }
    }

    #[instrument(level = "trace", skip(self), ret)]
    fn compute_const_arg_has_type_goal(
        &mut self,
        goal: Goal<'tcx, (ty::Const<'tcx>, Ty<'tcx>)>,
    ) -> QueryResult<'tcx> {
        let (ct, ty) = goal.predicate;

        let ct_ty = match ct.kind() {
            // FIXME: Ignore effect vars because canonicalization doesn't handle them correctly
            // and if we stall on the var then we wind up creating ambiguity errors in a probe
            // for this goal which contains an effect var. Which then ends up ICEing.
            ty::ConstKind::Infer(ty::InferConst::EffectVar(_)) => {
                return self.evaluate_added_goals_and_make_canonical_response(Certainty::Yes);
            }
            ty::ConstKind::Infer(_) => {
                return self.evaluate_added_goals_and_make_canonical_response(Certainty::AMBIGUOUS);
            }
            ty::ConstKind::Error(_) => {
                return self.evaluate_added_goals_and_make_canonical_response(Certainty::Yes);
            }
            ty::ConstKind::Unevaluated(uv) => {
                self.interner().type_of(uv.def).instantiate(self.interner(), uv.args)
            }
            ty::ConstKind::Expr(_) => unimplemented!(
                "`feature(generic_const_exprs)` is not supported in the new trait solver"
            ),
            ty::ConstKind::Param(_) => {
                unreachable!("`ConstKind::Param` should have been canonicalized to `Placeholder`")
            }
            ty::ConstKind::Bound(_, _) => bug!("escaping bound vars in {:?}", ct),
            ty::ConstKind::Value(ty, _) => ty,
            ty::ConstKind::Placeholder(placeholder) => {
                placeholder.find_const_ty_from_env(goal.param_env)
            }
        };

        self.eq(goal.param_env, ct_ty, ty)?;
        self.evaluate_added_goals_and_make_canonical_response(Certainty::Yes)
    }
}

impl<'tcx> EvalCtxt<'_, SolverDelegate<'tcx>> {
    /// Try to merge multiple possible ways to prove a goal, if that is not possible returns `None`.
    ///
    /// In this case we tend to flounder and return ambiguity by calling `[EvalCtxt::flounder]`.
    #[instrument(level = "trace", skip(self), ret)]
    fn try_merge_responses(
        &mut self,
        responses: &[CanonicalResponse<'tcx>],
    ) -> Option<CanonicalResponse<'tcx>> {
        if responses.is_empty() {
            return None;
        }

        // FIXME(-Znext-solver): We should instead try to find a `Certainty::Yes` response with
        // a subset of the constraints that all the other responses have.
        let one = responses[0];
        if responses[1..].iter().all(|&resp| resp == one) {
            return Some(one);
        }

        responses
            .iter()
            .find(|response| {
                response.value.certainty == Certainty::Yes
                    && response.has_no_inference_or_external_constraints()
            })
            .copied()
    }

    /// If we fail to merge responses we flounder and return overflow or ambiguity.
    #[instrument(level = "trace", skip(self), ret)]
    fn flounder(&mut self, responses: &[CanonicalResponse<'tcx>]) -> QueryResult<'tcx> {
        if responses.is_empty() {
            return Err(NoSolution);
        }

        let Certainty::Maybe(maybe_cause) =
            responses.iter().fold(Certainty::AMBIGUOUS, |certainty, response| {
                certainty.unify_with(response.value.certainty)
            })
        else {
            bug!("expected flounder response to be ambiguous")
        };

        Ok(self.make_ambiguous_response_no_constraints(maybe_cause))
    }

    /// Normalize a type for when it is structurally matched on.
    ///
    /// This function is necessary in nearly all cases before matching on a type.
    /// Not doing so is likely to be incomplete and therefore unsound during
    /// coherence.
    #[instrument(level = "trace", skip(self, param_env), ret)]
    fn structurally_normalize_ty(
        &mut self,
        param_env: ty::ParamEnv<'tcx>,
        ty: Ty<'tcx>,
    ) -> Result<Ty<'tcx>, NoSolution> {
        if let ty::Alias(..) = ty.kind() {
            let normalized_ty = self.next_ty_infer();
            let alias_relate_goal = Goal::new(
                self.interner(),
                param_env,
                ty::PredicateKind::AliasRelate(
                    ty.into(),
                    normalized_ty.into(),
                    AliasRelationDirection::Equate,
                ),
            );
            self.add_goal(GoalSource::Misc, alias_relate_goal);
            self.try_evaluate_added_goals()?;
            Ok(self.resolve_vars_if_possible(normalized_ty))
        } else {
            Ok(ty)
        }
    }
}

fn response_no_constraints_raw<I: Interner>(
    tcx: I,
    max_universe: UniverseIndex,
    variables: I::CanonicalVars,
    certainty: Certainty,
) -> ir::solve::CanonicalResponse<I> {
    ir::Canonical {
        max_universe,
        variables,
        value: Response {
            var_values: ir::CanonicalVarValues::make_identity(tcx, variables),
            // FIXME: maybe we should store the "no response" version in tcx, like
            // we do for tcx.types and stuff.
            external_constraints: tcx.mk_external_constraints(ExternalConstraintsData::default()),
            certainty,
        },
        defining_opaque_types: Default::default(),
    }
}
