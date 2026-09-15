//! Worker crate library modules shared by binaries.

pub const DEFAULT_SNAPSHOT_PROCESS_STATE_START: i32 = 100;
pub const DEFAULT_SNAPSHOT_PROCESS_STATE_END: i32 = 199;

/// The only state code that qualifies a public Process for numerical use.
///
/// `100..=199` is a reserved publication *display* segment, not a computation capability. Every
/// public numerical entrypoint therefore admits exactly this state; any other value, including a
/// newly reserved one such as an aggregated-Process state, fails closed.
pub const NUMERIC_ELIGIBLE_PUBLIC_PROCESS_STATE: i32 = 100;

/// Authorized owner-draft state retained for interactive owner builds.
///
/// This state is never sufficient on its own: the versioned owner predicate must also prove
/// `user_id = actor` for the row, and the row must be rechecked after it is materialized.
pub const OWNER_DRAFT_PROCESS_STATE: i32 = 0;

/// Reserved published-Result state.
///
/// A published Result is readable, referenceable and exportable, but it is never a numerical root,
/// provider or matrix axis. The literal is a cross-repository contract value owned by Database
/// #646; it must not drift from the Database check constraint and publication command.
pub const PUBLISHED_RESULT_PROCESS_STATE: i32 = 120;

/// Reserved in-review process state used by the Review Admin quality diagnostic only.
///
/// It is intentionally *not* part of any generic numerical default. `cargo solver` never dispatches
/// jobs of this family, but any generic caller that forwards this string must fail closed instead
/// of quietly widening the numerical universe.
pub const REVIEW_IN_PROGRESS_PROCESS_STATE: i32 = 20;

/// Artifact purpose that marks the dedicated Review Admin quality-diagnostic snapshot build.
pub const REVIEW_QUALITY_DIAGNOSTIC_SNAPSHOT_ARTIFACT_PURPOSE: &str = "review_quality_diagnostic";

/// Artifact purpose that marks the legacy offline review-submit overlay snapshot build.
pub const REVIEW_SUBMIT_OVERLAY_ARTIFACT_PURPOSE: &str = "review_submit_overlay";

/// Database eligibility predicate for the no-current-release candidate numerical universe.
///
/// Owned by Database #646 / the Scope Closure normalization path. Worker consumes the literal; it
/// must not be re-derived or relaxed locally.
pub const CANDIDATE_PUBLIC_NUMERICAL_PREDICATE_V2: &str = "candidate-public-state-code-100:v2";

/// Database eligibility predicate for the eligible-input manifest (latest revision per id).
pub const PUBLISHED_STATE_100_LATEST_PER_ID_PREDICATE_V2: &str =
    "published-state-code-100:latest-per-id:v2";

/// Existing formal-release predicate. Its membership rule is a separate contract and is unchanged.
pub const CURRENT_PUBLIC_RELEASE_MANIFEST_PREDICATE_V2: &str = "current-public-release-manifest:v2";

/// Versioned numerical-policy marker recorded in every newly built numerical snapshot.
///
/// This is the *global* policy identity of a numerical snapshot: the public numerical universe is
/// exactly state `100`, the reserved publication segment `101..199` carries no computation
/// capability, and the published Result state `120` is never a root, provider or matrix axis.
///
/// It is intentionally independent of the Scope Closure binding hash. A binding-specific hash can
/// only cover closure-bound snapshots, so it cannot fence ordinary global/subset snapshots, which
/// may have been built under the retired `100..199` membership. New compute execution and reuse
/// require this marker and fail closed when it is absent or stale; artifact decoding and historical
/// reads do not consult it.
///
/// The literal is Worker-owned and new in this change. It must be aligned with Database #646 before
/// any dependent integration relies on it.
pub const NUMERICAL_SNAPSHOT_POLICY_VERSION: &str =
    "public-numerical-state-100-excluding-result-120:v1";

/// Schema of the numerical source fingerprint that participates in snapshot/cache identity.
///
/// Bumped because the fingerprint now incorporates [`NUMERICAL_SNAPSHOT_POLICY_VERSION`], so a
/// fingerprint computed under an older policy can never match a new build or reuse lookup.
pub const NUMERICAL_SOURCE_FINGERPRINT_SCHEMA: &str = "source-fingerprint:v3";

/// True when a recorded numerical-policy marker is the current one.
///
/// A missing marker is *not* current: snapshots written before this policy existed fail closed for
/// new compute instead of inheriting eligibility evidence from the retired membership rule.
#[must_use]
pub fn is_current_numerical_snapshot_policy(value: Option<&str>) -> bool {
    value == Some(NUMERICAL_SNAPSHOT_POLICY_VERSION)
}

/// Versioned product-exposure policy for TIDAS package export.
///
/// A published Result Process must not leave the platform as a product package dataset through any
/// seed, exact-reference, latest-reference, cached/resumed traversal or final-hydration path.
///
/// This policy is deliberately *separate* from [`NUMERICAL_SNAPSHOT_POLICY_VERSION`]:
///
/// * it applies only to `processes` rows, so Flow/FlowProperty/UnitGroup/Source/Contact support data
///   keeps its existing `100..=199` export semantics unchanged;
/// * it changes no import conflict rule, so `state_code` handling on import is untouched;
/// * it is a Worker-side exposure fence, not a numerical eligibility rule.
///
/// The literal is Worker-owned and new in this change: no Database or Edge artifact defines it yet,
/// so it must be aligned with Database #646 (admission/candidate rules) and the Edge request
/// contracts before dependent integration relies on it.
pub const PRODUCT_EXPORT_POLICY_VERSION: &str = "product-export-excludes-published-result-120:v1";

/// True when a `processes` row may be emitted into a product export package.
///
/// Export stays inclusive: anything that is not the reserved published-Result state is still
/// exportable, so drafts, in-review rows and the reserved publication segment keep their current
/// behavior and only the decided Result state is withheld.
#[must_use]
pub fn is_product_exportable_process_state(state_code: Option<i32>) -> bool {
    state_code != Some(PUBLISHED_RESULT_PROCESS_STATE)
}

/// Retired numerical eligibility predicates that a *new* calculation must not accept as evidence.
pub const RETIRED_NUMERICAL_ELIGIBILITY_PREDICATES: [&str; 3] = [
    "published-state-code-100-199:v1",
    "candidate-public-state-code-100-199:v1",
    "published-state-code-100-199:latest-per-id:v1",
];

/// True when a Database eligibility predicate version still describes the current numerical rule.
#[must_use]
pub fn is_current_numerical_eligibility_predicate(value: &str) -> bool {
    matches!(
        value,
        CANDIDATE_PUBLIC_NUMERICAL_PREDICATE_V2
            | PUBLISHED_STATE_100_LATEST_PER_ID_PREDICATE_V2
            | CURRENT_PUBLIC_RELEASE_MANIFEST_PREDICATE_V2
    )
}

/// True when the predicate version is one of the retired reserved-range literals.
#[must_use]
pub fn is_retired_numerical_eligibility_predicate(value: &str) -> bool {
    RETIRED_NUMERICAL_ELIGIBILITY_PREDICATES.contains(&value)
}

/// Fails closed when new numerical execution is handed a retired Database eligibility predicate.
///
/// Historical artifacts and manifests stay readable through administrative readers; they simply
/// cannot be reused as eligibility evidence for a new calculation.
pub fn ensure_current_numerical_eligibility_predicate(actual: &str) -> anyhow::Result<()> {
    if is_current_numerical_eligibility_predicate(actual) {
        return Ok(());
    }
    if is_retired_numerical_eligibility_predicate(actual) {
        anyhow::bail!(
            "retired_numerical_eligibility_predicate: {actual} selected the reserved 100..199 segment; rebuild the scope under the current public numerical predicate"
        );
    }
    anyhow::bail!("numerical_eligibility_predicate_unknown: {actual}")
}

/// Public numerical eligibility predicate: exactly [`NUMERIC_ELIGIBLE_PUBLIC_PROCESS_STATE`].
///
/// Owner-draft authorization and administrative/export reads are deliberately outside this
/// predicate: they carry their own scoping rule and must never be expressed as a state range here.
#[must_use]
pub fn is_public_numerical_process_state(state_code: i32) -> bool {
    state_code == NUMERIC_ELIGIBLE_PUBLIC_PROCESS_STATE
}

/// Stable diagnostic reason for a Process that is not numerically eligible.
#[must_use]
pub fn numerical_ineligibility_reason(state_code: Option<i32>) -> &'static str {
    match state_code {
        Some(PUBLISHED_RESULT_PROCESS_STATE) => "published_result_process_is_not_a_numerical_input",
        Some(_) => "process_state_is_not_numerically_eligible",
        None => "process_state_unknown_for_numerical_input",
    }
}

/// Fails closed when a Process must not enter a numerical universe.
pub fn ensure_numerical_process_eligible(
    process_id: uuid::Uuid,
    process_version: &str,
    state_code: Option<i32>,
) -> anyhow::Result<()> {
    if state_code.is_some_and(is_public_numerical_process_state) {
        return Ok(());
    }
    anyhow::bail!(
        "{}: process {process_id}@{process_version} state_code={:?}",
        numerical_ineligibility_reason(state_code),
        state_code
    )
}

/// Sorted, deduplicated rendering of a process-state list.
#[must_use]
pub fn normalized_process_states(states: impl IntoIterator<Item = i32>) -> Vec<i32> {
    let mut states = states.into_iter().collect::<Vec<_>>();
    states.sort_unstable();
    states.dedup();
    states
}

/// Canonical sorted comma-separated process-state CLI/payload value.
#[must_use]
pub fn render_process_states_arg(states: impl IntoIterator<Item = i32>) -> String {
    normalized_process_states(states)
        .into_iter()
        .map(|state| state.to_string())
        .collect::<Vec<_>>()
        .join(",")
}

/// The single public numerical process-state set.
#[must_use]
pub fn numerical_process_states() -> Vec<i32> {
    vec![NUMERIC_ELIGIBLE_PUBLIC_PROCESS_STATE]
}

/// Canonical public numerical process-state argument: exactly `"100"`.
#[must_use]
pub fn numerical_process_states_arg() -> String {
    render_process_states_arg(numerical_process_states())
}

/// True when a raw process-state list is a non-empty comma-separated integer list.
#[must_use]
pub fn is_well_formed_process_states_arg(value: &str) -> bool {
    let trimmed = value.trim().replace(' ', "");
    !trimmed.is_empty() && trimmed.split(',').all(|token| token.parse::<i32>().is_ok())
}

/// Parses a process-state list argument, failing closed on any non-integer token.
///
/// `all`/empty is reported as `None`: it is an explicit "no state filter" request that only the
/// owning caller may interpret, never a silent widening of a numerical universe.
#[must_use]
pub fn parse_process_states_arg(value: &str) -> Option<Option<Vec<i32>>> {
    let normalized = value.trim().replace(' ', "");
    if normalized.is_empty() || normalized.eq_ignore_ascii_case("all") {
        return Some(None);
    }
    let mut parsed = Vec::new();
    for token in normalized.split(',') {
        match token.parse::<i32>() {
            Ok(state) => parsed.push(state),
            Err(_) => return None,
        }
    }
    Some(Some(normalized_process_states(parsed)))
}

/// True when a raw process-state list resolves to exactly the public numerical scope (`100`).
#[must_use]
pub fn is_numerical_process_states_arg(value: &str) -> bool {
    match parse_process_states_arg(value) {
        Some(Some(states)) => states == numerical_process_states(),
        _ => false,
    }
}

/// Canonical numerical scope for the Review Admin quality diagnostic.
///
/// The diagnostic deliberately spans in-review (`20`) and public (`100`) Processes only. The
/// reserved publication segment `101..199`, including the published Result state `120`, stays out
/// even here: a published Result is never a diagnostic matrix axis, so this dedicated exception
/// cannot reopen the numerical boundary.
#[must_use]
pub fn review_quality_diagnostic_process_states_arg() -> String {
    render_process_states_arg(review_quality_diagnostic_process_states())
}

/// True when a caller's process-state list is exactly the review-only diagnostic scope.
///
/// The comparison is exact so an arbitrary caller cannot label a widened list as the review
/// exception.
#[must_use]
pub fn is_review_quality_diagnostic_process_states_arg(value: &str) -> bool {
    match parse_process_states_arg(value) {
        Some(Some(states)) => is_review_quality_diagnostic_process_states(states.as_slice()),
        _ => false,
    }
}

/// Canonical review-diagnostic state set: the in-review state `20` plus the public numerical
/// state `100`.
///
/// The dedicated Review Admin diagnostic is the only intentional numeric exception. Its public
/// half is the same exact-100 public state as everywhere else, so the reserved publication segment
/// and the published Result state stay out of the diagnostic matrix too.
#[must_use]
pub fn review_quality_diagnostic_process_states() -> Vec<i32> {
    normalized_process_states([
        REVIEW_IN_PROGRESS_PROCESS_STATE,
        NUMERIC_ELIGIBLE_PUBLIC_PROCESS_STATE,
    ])
}

/// Default process states for the `snapshot-builder` CLI: exactly the public numerical state.
///
/// The offline default is the same fail-closed boundary as the versioned `public_plus_owner_draft`
/// scope, whose public clause is exactly state `100`. A generic builder may additionally join
/// actor-scoped owner drafts through the versioned scope path, but it must never derive owner
/// drafts from the bare state value `0`.
#[must_use]
pub fn default_snapshot_process_states_arg() -> String {
    numerical_process_states_arg()
}

/// True when a process-state list is exactly the dedicated review-diagnostic scope.
#[must_use]
pub fn is_review_quality_diagnostic_process_states(states: &[i32]) -> bool {
    normalized_process_states(states.iter().copied()) == review_quality_diagnostic_process_states()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_numerical_eligibility_is_exactly_one_hundred() {
        for state in [0, 20, 99, 101, 120, 150, 199, 200] {
            assert!(!is_public_numerical_process_state(state), "state {state}");
        }
        assert!(is_public_numerical_process_state(100));
    }

    #[test]
    fn default_numerical_process_states_are_exactly_the_public_state() {
        assert_eq!(numerical_process_states(), vec![100]);
        assert_eq!(numerical_process_states_arg(), "100");
        assert_eq!(default_snapshot_process_states_arg(), "100");
        assert!(!default_snapshot_process_states_arg().contains("199"));
    }

    #[test]
    fn review_diagnostic_scope_is_dedicated_and_excludes_the_result_state() {
        let review = review_quality_diagnostic_process_states_arg();
        let states = review_quality_diagnostic_process_states();

        assert_eq!(states, vec![20, 100]);
        assert_eq!(review, "20,100");
        assert_eq!(
            review,
            review_quality_diagnostic_process_states_arg(),
            "the renderer and the recognizer must accept one identical literal"
        );
        assert!(!states.contains(&PUBLISHED_RESULT_PROCESS_STATE));
        assert!(is_review_quality_diagnostic_process_states_arg(
            review.as_str()
        ));
        // Ordinary public scopes can never be mistaken for the review exception.
        for value in ["100", "20", "100,101", "all", "", "20,101", "20,120"] {
            assert!(
                !is_review_quality_diagnostic_process_states_arg(value),
                "{value}"
            );
        }
        // A widened review list is a different scope and is not the exception.
        let mut widened = vec![20, 100];
        widened.push(120);
        assert!(!is_review_quality_diagnostic_process_states_arg(
            render_process_states_arg(widened).as_str()
        ));
    }

    #[test]
    fn published_result_state_reports_its_own_diagnostic_and_fails_closed() {
        assert_eq!(
            numerical_ineligibility_reason(Some(PUBLISHED_RESULT_PROCESS_STATE)),
            "published_result_process_is_not_a_numerical_input"
        );
        assert_eq!(
            numerical_ineligibility_reason(Some(150)),
            "process_state_is_not_numerically_eligible"
        );
        assert_eq!(
            numerical_ineligibility_reason(None),
            "process_state_unknown_for_numerical_input"
        );

        let process_id = uuid::Uuid::nil();
        let error = ensure_numerical_process_eligible(
            process_id,
            "01.00.000",
            Some(PUBLISHED_RESULT_PROCESS_STATE),
        )
        .expect_err("published Result must fail closed");
        assert!(
            error
                .to_string()
                .contains("published_result_process_is_not_a_numerical_input")
        );
        ensure_numerical_process_eligible(process_id, "01.00.000", Some(100))
            .expect("state 100 is numerically eligible");
        // Owner drafts and reserved public states are not public numerical inputs by state alone.
        for state in [0, 20, 101, 150, 199, 200] {
            assert!(
                ensure_numerical_process_eligible(process_id, "01.00.000", Some(state)).is_err(),
                "state {state}"
            );
        }
        assert!(ensure_numerical_process_eligible(process_id, "01.00.000", None).is_err());
    }

    #[test]
    fn process_state_argument_shape_is_checked_before_scoping() {
        assert!(is_well_formed_process_states_arg("100"));
        assert!(is_well_formed_process_states_arg(" 100 , 101 "));
        assert!(!is_well_formed_process_states_arg(""));
        assert!(!is_well_formed_process_states_arg("all"));
        assert!(!is_well_formed_process_states_arg("100,abc"));
    }

    #[test]
    fn retired_database_eligibility_predicates_fail_closed_for_new_execution() {
        for current in [
            CANDIDATE_PUBLIC_NUMERICAL_PREDICATE_V2,
            PUBLISHED_STATE_100_LATEST_PER_ID_PREDICATE_V2,
            CURRENT_PUBLIC_RELEASE_MANIFEST_PREDICATE_V2,
        ] {
            ensure_current_numerical_eligibility_predicate(current)
                .unwrap_or_else(|error| panic!("{current}: {error}"));
        }
        for retired in RETIRED_NUMERICAL_ELIGIBILITY_PREDICATES {
            let error = ensure_current_numerical_eligibility_predicate(retired)
                .expect_err("retired reserved-range predicate must not be reused");
            assert!(
                error
                    .to_string()
                    .contains("retired_numerical_eligibility_predicate"),
                "{retired}: {error}"
            );
            assert!(is_retired_numerical_eligibility_predicate(retired));
        }
        let error = ensure_current_numerical_eligibility_predicate("something-else:v1")
            .expect_err("unknown predicate must fail closed");
        assert!(
            error
                .to_string()
                .contains("numerical_eligibility_predicate_unknown")
        );
    }
}

pub mod ai;
pub mod artifact_gc;
pub mod artifacts;
pub mod calculation_bundle;
pub mod calculation_evidence;
pub mod compiled_graph;
pub mod config;
pub mod contribution_path;
pub mod db;
pub mod db_pool;
pub mod file_cache;
pub mod graph_types;
pub mod http;
pub mod local_reports;
pub mod package_artifacts;
pub mod package_db;
pub mod package_execution;
pub mod package_retention;
pub mod package_types;
pub mod pgbouncer_sqlx;
pub mod portal_lcia_projection;
pub mod queue;
pub mod readiness;
pub mod resource;
pub mod review_quality_diagnostic_runner;
pub mod review_submit_gate;
pub mod scope_closure;
pub mod signed_flow;
pub mod snapshot_artifacts;
pub mod snapshot_builder_protocol;
pub mod snapshot_index;
pub mod snapshot_retention;
pub mod snapshot_source_closure;
pub mod source_reference_policy;
pub mod static_lcia_cache;
pub mod storage;
pub mod tidas_cli;
pub mod tidas_process_semantics;
pub mod types;
pub mod worker_jobs;
