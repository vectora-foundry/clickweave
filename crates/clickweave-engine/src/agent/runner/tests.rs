// Test modules split out of runner/mod.rs to keep production code navigable.

use super::*;

mod datetime_oracle_executor_tests;

mod builder_tests;

mod observe_tests;

mod turn_application_tests;

mod continuity_tests;

mod ax_enrichment_tests;

mod storage_persistence_tests;

mod agent_turn_parsing_tests;

mod parse_agent_turn_tool_calls_tests;

mod unverified_side_effect_guard_tests;

mod no_progress_guard_tests;

mod invalidation_wiring_tests;

mod source_agnostic_elements_tests;

mod resolve_cdp_target_tests;

mod focus_skip_tests;

/// Coordinate-primitive guard: defense-in-depth check that a wrong-family
/// dispatch (`click` / `type_text` / `press_key` / `move_mouse` / `scroll`
/// / `drag`) is rejected at the harness layer when a structured surface
/// (`cdp_page` for CDP-backed apps, `take_ax_snapshot` + AX dispatch for
/// Native) is wired for the focused app. Sits behind the per-turn
/// `<tools_in_scope>` filter — these tests pin the predicate alone; the
/// dispatch-site behaviour (synthetic StepOutcome::Error, StepFailed
/// event, recovery_strategy interaction) is covered by the integration
/// suite.
mod coordinate_primitive_guard_tests;

/// CDP auto-connect status field (`world_model.cdp_connect_status`).
/// The runner sets this whenever `auto_connect_cdp` exhausts retries
/// and clears it on success or focus change. Without the field, the
/// LLM cannot tell "auto-connect hasn't fired yet" (no cdp_page, no
/// status) from "auto-connect tried and failed permanently" (no
/// cdp_page, status present).
mod cdp_connect_status_tests;

/// D24/D29 run-start retrieval gate + step_index ownership tests.
/// The gate (`episodic_run_start_retrieved`) replaces the drift-prone
/// `step_index == 0` proxy; the helper (`advance_recorded_step_index`)
/// is the single owner of `step_index` updates so the counter matches
/// `state.steps.len()` across all recording paths (synthetic skip,
/// policy deny, approval reject, normal LLM turn).
mod retrieval_gate_tests;

mod skills_apply_mutations_tests;

mod dispatch_skill_tests;
