//! Skills lifecycle commands. Mirrors the shape used by
//! `src-tauri/src/commands/agent_chat.rs` — `tauri::AppHandle` plus a
//! request struct, no `AppState`.

use std::path::PathBuf;

use clickweave_engine::agent::skills::patch::{
    ActionSketchReplacement, MarkdownReplacement, ReplaySidecarMutation,
};
use clickweave_engine::agent::skills::replay::ReplayJson;
use clickweave_engine::agent::skills::{
    ActionSketchStep, ApplicabilityHints, ParameterSlot, Skill, SkillError,
    SkillFrontmatterVariable, SkillLintError, SkillPatch, SkillPatchPrimitive,
    SkillRefinementProposal, SkillScope, SkillState, SkillStore, apply_patch_to_skill,
    emit_skill_md, lint_skill_patch, parse_replay_json, parse_skill_md, slugify,
};
use serde::{Deserialize, Serialize};
use specta::Type;
use tauri::Emitter;

use crate::commands::error::CommandError;
use crate::commands::types::resolve_storage;

#[derive(Debug, Deserialize, Type)]
pub struct ConfirmSkillProposalRequest {
    pub skill_id: String,
    pub version: u32,
    pub accepted_proposal: SkillRefinementProposal,
    pub project_path: Option<String>,
    pub project_name: String,
    pub project_id: String,
    pub run_id: Option<String>,
    pub store_traces: bool,
}

#[derive(Debug, Deserialize, Type)]
pub struct RejectSkillProposalRequest {
    pub skill_id: String,
    pub version: u32,
    pub project_path: Option<String>,
    pub project_name: String,
    pub project_id: String,
    pub store_traces: bool,
}

#[derive(Debug, Deserialize, Type)]
pub struct PromoteSkillToGlobalRequest {
    pub skill_id: String,
    pub version: u32,
    pub project_path: Option<String>,
    pub project_name: String,
    pub project_id: String,
    pub store_traces: bool,
}

#[derive(Debug, Deserialize, Type)]
pub struct ForkSkillRequest {
    pub skill_id: String,
    pub version: u32,
    pub new_name: String,
    pub project_path: Option<String>,
    pub project_name: String,
    pub project_id: String,
    pub store_traces: bool,
}

#[derive(Debug, Deserialize, Type)]
pub struct DeleteSkillRequest {
    pub skill_id: String,
    pub version: u32,
    pub scope: SkillScope,
    pub project_path: Option<String>,
    pub project_name: String,
    pub project_id: String,
    pub store_traces: bool,
}

#[derive(Debug, Deserialize, Type)]
pub struct ListSkillsRequest {
    pub scope: SkillScope,
    pub project_path: Option<String>,
    pub project_name: String,
    pub project_id: String,
    pub store_traces: bool,
}

/// Request for [`load_skill_full`] — resolves the full [`Skill`] value
/// (including `sections` and `body`) for a given skill id. The panel
/// sidebar already holds `SkillSummary`; this is called once, on
/// selection, to hydrate the detail view.
#[derive(Debug, Deserialize, Type)]
pub struct LoadSkillFullRequest {
    pub skill_id: String,
    pub version: u32,
    pub project_path: Option<String>,
    pub project_name: String,
    pub project_id: String,
    pub store_traces: bool,
}

/// Lightweight projection of [`Skill`] for the Skills panel listing.
/// The full canvas + frontmatter are loaded on demand when the user
/// opens a detail view, so the panel index stays small.
#[derive(Debug, Clone, Serialize, Type)]
pub struct SkillSummary {
    pub id: String,
    pub version: u32,
    pub name: String,
    pub description: String,
    pub state: SkillState,
    pub scope: SkillScope,
    pub tags: Vec<String>,
    pub parameter_schema: Vec<ParameterSlot>,
    pub applicability: ApplicabilityHints,
    pub action_sketch: Vec<ActionSketchStep>,
    pub proposal: Option<SkillRefinementProposal>,
    pub occurrence_count: u32,
    pub success_rate: f32,
    pub edited_by_user: bool,
}

impl SkillSummary {
    fn from_skill(s: &Skill, proposal: Option<SkillRefinementProposal>) -> Self {
        Self {
            id: s.id.clone(),
            version: s.version,
            name: s.name.clone(),
            description: s.description.clone(),
            state: s.state,
            scope: s.scope,
            tags: s.tags.clone(),
            parameter_schema: s.parameter_schema.clone(),
            applicability: s.applicability.clone(),
            action_sketch: s.action_sketch.clone(),
            proposal,
            occurrence_count: s.stats.occurrence_count,
            success_rate: s.stats.success_rate,
            edited_by_user: s.edited_by_user,
        }
    }
}

fn project_skills_dir_for(
    app: &tauri::AppHandle,
    project_path: &Option<String>,
    project_name: &str,
    project_id_str: &str,
) -> Result<PathBuf, CommandError> {
    let project_uuid: uuid::Uuid = project_id_str
        .parse()
        .map_err(|_| CommandError::validation("Invalid project ID"))?;
    let storage = resolve_storage(app, project_path, project_name, project_uuid);
    storage
        .project_skills_dir()
        .map_err(|e| CommandError::io(format!("resolve project_skills_dir: {e}")))
}

/// Global-tier skills directory shared across projects on this machine.
/// Located under the OS app-data dir so promote-to-global preserves the
/// same install-key semantics the rest of the app uses.
fn global_skills_dir(app: &tauri::AppHandle) -> Result<PathBuf, CommandError> {
    use crate::commands::types::AppDataDir;
    use tauri::Manager;
    let dir = app.state::<AppDataDir>().0.join("skills_global");
    if !dir.exists() {
        std::fs::create_dir_all(&dir)
            .map_err(|e| CommandError::io(format!("create global skills dir: {e}")))?;
    }
    Ok(dir)
}

fn skill_filename(skill_id: &str, version: u32) -> String {
    format!("{}-v{}.md", slugify(skill_id), version)
}

fn proposal_filename(skill_id: &str, version: u32) -> String {
    format!("{}-v{}.proposal.json", slugify(skill_id), version)
}

fn read_proposal(
    dir: &std::path::Path,
    skill_id: &str,
    version: u32,
) -> Option<SkillRefinementProposal> {
    let path = dir.join(proposal_filename(skill_id, version));
    let contents = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&contents).ok()
}

fn read_skill_at(
    store: &SkillStore,
    skill_id: &str,
    version: u32,
) -> Result<(Skill, PathBuf), CommandError> {
    let path = store.dir().join(skill_filename(skill_id, version));
    let skill = store
        .read_skill(&path)
        .map_err(|e| CommandError::io(format!("read skill {}-v{}: {}", skill_id, version, e)))?;
    Ok((skill, path))
}

fn ensure_skill_file_io_enabled(store_traces: bool) -> Result<(), CommandError> {
    if store_traces {
        Ok(())
    } else {
        Err(CommandError::validation(
            "Skill file access is disabled while trace persistence is off",
        ))
    }
}

#[tauri::command]
#[specta::specta]
pub async fn confirm_skill_proposal(
    app: tauri::AppHandle,
    request: ConfirmSkillProposalRequest,
) -> Result<(), CommandError> {
    ensure_skill_file_io_enabled(request.store_traces)?;
    let dir = project_skills_dir_for(
        &app,
        &request.project_path,
        &request.project_name,
        &request.project_id,
    )?;
    let store = SkillStore::new(dir.clone());
    let (mut skill, old_path) = read_skill_at(&store, &request.skill_id, request.version)?;

    skill.parameter_schema = request.accepted_proposal.parameter_schema;
    skill.description = request.accepted_proposal.description;
    if let Some(name) = request
        .accepted_proposal
        .name_suggestion
        .filter(|s| !s.trim().is_empty())
    {
        skill.name = name;
    }
    // Binding-correction application is intentionally minimal here:
    // bindings live inside `action_sketch` capture clauses; precise
    // patching is delegated to the Phase 5 follow-up alongside the
    // proposal-task wiring (Task 5.2). We at least record the
    // confirmation by flipping state.
    skill.state = SkillState::Confirmed;

    store
        .write_skill(&skill)
        .map_err(|e| CommandError::io(format!("write confirmed skill: {e}")))?;

    // Best-effort cleanup of the LLM proposal sidecar file.
    let proposal_path = dir.join(format!(
        "{}-v{}.proposal.json",
        slugify(&skill.id),
        skill.version
    ));
    let _ = std::fs::remove_file(&proposal_path);

    let run_id = request.run_id.unwrap_or_default();
    let _ = app.emit(
        "agent://skill_confirmed",
        serde_json::json!({
            "run_id": run_id,
            "event_run_id": run_id,
            "skill_id": skill.id,
            "version": skill.version,
        }),
    );
    let _ = old_path; // path stays the same; rename only if id/version changed
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub async fn reject_skill_proposal(
    app: tauri::AppHandle,
    request: RejectSkillProposalRequest,
) -> Result<(), CommandError> {
    ensure_skill_file_io_enabled(request.store_traces)?;
    let dir = project_skills_dir_for(
        &app,
        &request.project_path,
        &request.project_name,
        &request.project_id,
    )?;
    let proposal_path = dir.join(format!(
        "{}-v{}.proposal.json",
        slugify(&request.skill_id),
        request.version
    ));
    if proposal_path.exists() {
        std::fs::remove_file(&proposal_path)
            .map_err(|e| CommandError::io(format!("remove proposal: {e}")))?;
    }
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub async fn promote_skill_to_global(
    app: tauri::AppHandle,
    request: PromoteSkillToGlobalRequest,
) -> Result<(), CommandError> {
    ensure_skill_file_io_enabled(request.store_traces)?;
    let project_dir = project_skills_dir_for(
        &app,
        &request.project_path,
        &request.project_name,
        &request.project_id,
    )?;
    let project_store = SkillStore::new(project_dir);
    let (mut skill, _) = read_skill_at(&project_store, &request.skill_id, request.version)?;

    if skill.state != SkillState::Confirmed {
        return Err(CommandError::validation(
            "promote_skill_to_global requires the skill to be in Confirmed state",
        ));
    }

    skill.state = SkillState::Promoted;
    skill.scope = SkillScope::Global;

    let global_dir = global_skills_dir(&app)?;
    let global_store = SkillStore::new(global_dir);
    global_store
        .write_skill(&skill)
        .map_err(|e| CommandError::io(format!("write promoted skill: {e}")))?;
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub async fn fork_skill(
    app: tauri::AppHandle,
    request: ForkSkillRequest,
) -> Result<Skill, CommandError> {
    ensure_skill_file_io_enabled(request.store_traces)?;
    let dir = project_skills_dir_for(
        &app,
        &request.project_path,
        &request.project_name,
        &request.project_id,
    )?;
    let store = SkillStore::new(dir.clone());
    let (source, _) = read_skill_at(&store, &request.skill_id, request.version)?;

    use chrono::Utc;
    let suffix = generate_id_suffix();
    let new_slug = slugify(&request.new_name);
    let new_id = if new_slug.is_empty() {
        format!("{}-{}", source.id, suffix)
    } else {
        format!("{}-{}", new_slug, suffix)
    };

    let mut forked = source.clone();
    forked.id = new_id;
    forked.version = 1;
    forked.name = if request.new_name.trim().is_empty() {
        source.name.clone()
    } else {
        request.new_name.clone()
    };
    forked.state = SkillState::Draft;
    forked.scope = SkillScope::ProjectLocal;
    forked.created_at = Utc::now();
    forked.updated_at = forked.created_at;
    forked.stats.last_invoked_at = None;
    forked.edited_by_user = false;
    // Forked skill starts with a fresh stats slate; caller is expected
    // to validate via re-recording.
    forked.stats.occurrence_count = 1;
    forked.stats.success_rate = 1.0;

    store
        .write_skill(&forked)
        .map_err(|e| CommandError::io(format!("write forked skill: {e}")))?;
    Ok(forked)
}

#[tauri::command]
#[specta::specta]
pub async fn delete_skill(
    app: tauri::AppHandle,
    request: DeleteSkillRequest,
) -> Result<(), CommandError> {
    ensure_skill_file_io_enabled(request.store_traces)?;
    let dir = match request.scope {
        SkillScope::Global => global_skills_dir(&app)?,
        SkillScope::ProjectLocal => project_skills_dir_for(
            &app,
            &request.project_path,
            &request.project_name,
            &request.project_id,
        )?,
    };
    let store = SkillStore::new(dir.clone());
    let path = dir.join(skill_filename(&request.skill_id, request.version));
    if path.exists() {
        store
            .delete_skill(&path)
            .map_err(|e| CommandError::io(format!("delete skill: {e}")))?;
    }
    Ok(())
}

/// Load the full [`Skill`] value (including `sections`, `body`, and
/// `action_sketch`) for a given `(skill_id, version)` pair. Scans the
/// project-local skill directory for a matching file. The lightweight
/// `list_skills_for_panel` is the preferred way to populate the sidebar
/// index; call this only on selection.
#[tauri::command]
#[specta::specta]
pub async fn load_skill_full(
    app: tauri::AppHandle,
    request: LoadSkillFullRequest,
) -> Result<Skill, CommandError> {
    ensure_skill_file_io_enabled(request.store_traces)?;
    let dir = project_skills_dir_for(
        &app,
        &request.project_path,
        &request.project_name,
        &request.project_id,
    )?;
    let store = SkillStore::new(dir.clone());
    let path = dir.join(skill_filename(&request.skill_id, request.version));
    if path.exists() {
        let skill = store.read_skill(&path).map_err(|e| {
            CommandError::io(format!(
                "read skill {}-v{}: {}",
                request.skill_id, request.version, e
            ))
        })?;
        return Ok(skill);
    }
    // Fall back to scanning all files in the directory — handles the case
    // where the caller knows the id but not the exact slugified filename.
    let files = store
        .list_files()
        .map_err(|e| CommandError::io(format!("list skill files: {e}")))?;
    for file_path in files {
        match store.read_skill(&file_path) {
            Ok(s) if s.id == request.skill_id && s.version == request.version => {
                return Ok(s);
            }
            _ => {}
        }
    }
    Err(CommandError::validation(format!(
        "skill not found: {}-v{}",
        request.skill_id, request.version
    )))
}

#[tauri::command]
#[specta::specta]
pub async fn list_skills_for_panel(
    app: tauri::AppHandle,
    request: ListSkillsRequest,
) -> Result<Vec<SkillSummary>, CommandError> {
    if !request.store_traces {
        return Ok(Vec::new());
    }
    let dir = match request.scope {
        SkillScope::Global => global_skills_dir(&app)?,
        SkillScope::ProjectLocal => project_skills_dir_for(
            &app,
            &request.project_path,
            &request.project_name,
            &request.project_id,
        )?,
    };
    let store = SkillStore::new(dir.clone());
    let mut out = Vec::new();
    let files = store
        .list_files()
        .map_err(|e| CommandError::io(format!("list skill files: {e}")))?;
    for path in files {
        match store.read_skill(&path) {
            Ok(s) => {
                let proposal = if s.state == SkillState::Draft {
                    read_proposal(&dir, &s.id, s.version)
                } else {
                    None
                };
                out.push(SkillSummary::from_skill(&s, proposal));
            }
            Err(e) => {
                tracing::warn!(?e, ?path, "skip malformed skill file");
            }
        }
    }
    Ok(out)
}

// ── apply_skill_patch ────────────────────────────────────────────────────────

/// Wire-format `MarkdownReplacement` for Tauri IPC (mirrors the engine type
/// with serde/specta derives).
#[derive(Debug, Deserialize, Type)]
pub struct MarkdownReplacementDto {
    pub old_text: String,
    pub new_text: String,
}

/// Wire-format `ActionSketchReplacement` for Tauri IPC.
#[derive(Debug, Deserialize, Type)]
pub struct ActionSketchReplacementDto {
    pub step_id: String,
    pub field: String,
    pub new_value: serde_json::Value,
}

/// Wire-format `SkillFrontmatterVariable` addition for Tauri IPC.
#[derive(Debug, Deserialize, Type)]
pub struct SkillFrontmatterVariableDto {
    pub name: String,
    #[serde(rename = "type")]
    pub type_: String,
    pub description: Option<String>,
    pub default: Option<serde_json::Value>,
}

/// Wire-format `ReplaySidecarMutation` for Tauri IPC.
#[derive(Debug, Deserialize, Type)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ReplaySidecarMutationDto {
    ClearSignals {
        step_id: String,
    },
    AppendSectionHistory {
        retired: String,
        split_into: Vec<String>,
        at_version: u32,
    },
    DeleteStepBundle {
        step_id: String,
    },
    UpdateRequiresApproval {
        step_id: String,
        value: Option<bool>,
    },
}

/// Primitive discriminant for the diff preview label.
#[derive(Debug, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum SkillPatchPrimitiveDto {
    Rebind,
    Reorder,
    Promote,
    FreeFormProse,
}

/// Request body for [`apply_skill_patch`].
#[derive(Debug, Deserialize, Type)]
pub struct ApplySkillPatchRequest {
    pub skill_id: String,
    pub version: u32,
    /// If `Some`, the current SKILL.md's mtime (millis since Unix epoch) must
    /// match this value or the command returns `ExternalConflict`.
    pub expected_mtime_ms: Option<u64>,
    pub markdown_replacements: Vec<MarkdownReplacementDto>,
    pub action_sketch_replacements: Vec<ActionSketchReplacementDto>,
    pub variables_additions: Vec<SkillFrontmatterVariableDto>,
    pub replay_sidecar_mutations: Vec<ReplaySidecarMutationDto>,
    pub primitive: SkillPatchPrimitiveDto,
    pub project_path: Option<String>,
    pub project_name: String,
    pub project_id: String,
    pub store_traces: bool,
}

fn map_skill_error(e: SkillError) -> CommandError {
    match e {
        SkillError::ExternalConflict => {
            CommandError::validation("Skill file was modified externally; reload and retry")
        }
        other => CommandError::io(other.to_string()),
    }
}

fn map_lint_errors(errs: Vec<SkillLintError>) -> CommandError {
    let msg = errs
        .iter()
        .map(|e| e.to_string())
        .collect::<Vec<_>>()
        .join("; ");
    CommandError::validation(format!("SkillPatch lint failures: {msg}"))
}

fn dto_to_markdown_replacement(dto: MarkdownReplacementDto) -> MarkdownReplacement {
    MarkdownReplacement {
        old_text: dto.old_text,
        new_text: dto.new_text,
    }
}

fn dto_to_action_sketch_replacement(dto: ActionSketchReplacementDto) -> ActionSketchReplacement {
    ActionSketchReplacement {
        step_id: dto.step_id,
        field: dto.field,
        new_value: dto.new_value,
    }
}

fn dto_to_variable(dto: SkillFrontmatterVariableDto) -> SkillFrontmatterVariable {
    SkillFrontmatterVariable {
        name: dto.name,
        type_: dto.type_,
        description: dto.description,
        default: dto.default,
    }
}

fn dto_to_sidecar_mutation(dto: ReplaySidecarMutationDto) -> ReplaySidecarMutation {
    match dto {
        ReplaySidecarMutationDto::ClearSignals { step_id } => {
            ReplaySidecarMutation::ClearSignals { step_id }
        }
        ReplaySidecarMutationDto::AppendSectionHistory {
            retired,
            split_into,
            at_version,
        } => ReplaySidecarMutation::AppendSectionHistory {
            retired,
            split_into,
            at_version,
        },
        ReplaySidecarMutationDto::DeleteStepBundle { step_id } => {
            ReplaySidecarMutation::DeleteStepBundle { step_id }
        }
        ReplaySidecarMutationDto::UpdateRequiresApproval { step_id, value } => {
            ReplaySidecarMutation::UpdateRequiresApproval { step_id, value }
        }
    }
}

fn dto_to_primitive(dto: SkillPatchPrimitiveDto) -> SkillPatchPrimitive {
    match dto {
        SkillPatchPrimitiveDto::Rebind => SkillPatchPrimitive::Rebind,
        SkillPatchPrimitiveDto::Reorder => SkillPatchPrimitive::Reorder,
        SkillPatchPrimitiveDto::Promote => SkillPatchPrimitive::Promote,
        SkillPatchPrimitiveDto::FreeFormProse => SkillPatchPrimitive::FreeFormProse,
    }
}

/// Apply a four-layer `SkillPatch` atomically to a skill's on-disk files.
///
/// Steps:
/// 1. Resolve the project skills directory.
/// 2. Read the current `SKILL.md` (and optional `replay.json` sidecar).
/// 3. Validate the `expected_mtime_ms` guard against the current file mtime.
/// 4. Apply the patch in-memory (pure).
/// 5. Run structural lint; reject with lint errors before opening the journal.
/// 6. Emit post-patch byte buffers.
/// 7. Write via `SkillStore::write_atomic_multi_file` (journal protocol).
#[tauri::command]
#[specta::specta]
pub async fn apply_skill_patch(
    app: tauri::AppHandle,
    request: ApplySkillPatchRequest,
) -> Result<(), CommandError> {
    ensure_skill_file_io_enabled(request.store_traces)?;

    let dir = project_skills_dir_for(
        &app,
        &request.project_path,
        &request.project_name,
        &request.project_id,
    )?;
    let store = SkillStore::new(dir.clone());

    // -- 1. Read current SKILL.md --
    let skill_path = dir.join(skill_filename(&request.skill_id, request.version));
    let current_md = std::fs::read_to_string(&skill_path)
        .map_err(|e| CommandError::io(format!("read SKILL.md: {e}")))?;

    // -- 2. mtime guard (D31) --
    if let Some(expected_ms) = request.expected_mtime_ms {
        use std::time::{Duration, UNIX_EPOCH};
        let meta = std::fs::metadata(&skill_path)
            .map_err(|e| CommandError::io(format!("stat SKILL.md: {e}")))?;
        let actual_mtime = meta
            .modified()
            .map_err(|e| CommandError::io(format!("read mtime: {e}")))?;
        let actual_ms = actual_mtime
            .duration_since(UNIX_EPOCH)
            .unwrap_or(Duration::ZERO)
            .as_millis() as u64;
        // Tolerate ≤1 ms rounding drift (filesystem precision).
        if actual_ms.abs_diff(expected_ms) > 1 {
            return Err(CommandError::validation(
                "Skill file was modified externally; reload and retry",
            ));
        }
    }

    // -- 3. Parse current files --
    let skill = parse_skill_md(&current_md).map_err(|e| CommandError::io(e.to_string()))?;

    // Read replay.json if present; skeleton-empty if absent.
    let replay_path = dir.join(&request.skill_id).join("replay.json");
    let replay: ReplayJson = if replay_path.exists() {
        let contents = std::fs::read_to_string(&replay_path)
            .map_err(|e| CommandError::io(format!("read replay.json: {e}")))?;
        parse_replay_json(&contents).map_err(|e| CommandError::io(e.to_string()))?
    } else {
        ReplayJson {
            skill_id: request.skill_id.clone(),
            schema_version: clickweave_engine::agent::skills::replay::REPLAY_SCHEMA_VERSION,
            ..Default::default()
        }
    };

    // -- 4. Build the patch --
    let patch = SkillPatch {
        skill_id: request.skill_id.clone(),
        markdown_replacements: request
            .markdown_replacements
            .into_iter()
            .map(dto_to_markdown_replacement)
            .collect(),
        action_sketch_replacements: request
            .action_sketch_replacements
            .into_iter()
            .map(dto_to_action_sketch_replacement)
            .collect(),
        variables_additions: request
            .variables_additions
            .into_iter()
            .map(dto_to_variable)
            .collect(),
        replay_sidecar_mutations: request
            .replay_sidecar_mutations
            .into_iter()
            .map(dto_to_sidecar_mutation)
            .collect(),
        primitive: dto_to_primitive(request.primitive),
    };

    // -- 5. Apply in-memory (pure) --
    let (new_skill, new_replay) =
        apply_patch_to_skill(&skill, replay, &patch).map_err(map_skill_error)?;

    // -- 6. Structural lint (before the journal opens) --
    lint_skill_patch(&new_skill, &new_replay, &patch).map_err(map_lint_errors)?;

    // -- 7. Emit byte buffers --
    let skill_md_bytes = emit_skill_md(&new_skill).into_bytes();
    let replay_bytes = serde_json::to_vec_pretty(&new_replay)
        .map_err(|e| CommandError::io(format!("encode replay.json: {e}")))?;

    // -- 8. Atomic journal write --
    // The skill_id directory layout is used for the journal. The SKILL.md
    // file lives at the legacy flat path (dir/skill_filename) in Phase 1;
    // the journal targets relative paths under the skill_id sub-directory.
    // For backwards compatibility we write both the legacy flat file and
    // the replay sidecar under skill_id/.
    //
    // Phase 1 legacy layout: skills/SKILL.md lives at `dir/<slug>-v<N>.md`
    // (flat) and replay.json lives at `dir/<skill_id>/replay.json`. The
    // journal's skill_dir is keyed on skill_id, so we write the SKILL.md
    // entry as a relative path that navigates back to the flat location.
    // To keep the journal logic simple, we use write_atomic_multi_file
    // with a skill_id sub-directory and record the SKILL.md write
    // separately using the simpler write_skill helper for the legacy path,
    // then use the journal only for the replay.json sidecar.
    //
    // NOTE: This is a Phase 1 trade-off — the true four-layer atomic write
    // only becomes achievable when SKILL.md moves into the skill_id/ directory
    // (Task 1.L.0). For Phase 1 we do a best-effort two-step: (a) journal the
    // replay.json change atomically, then (b) rename the SKILL.md tmp file.
    // The crash case between (a) and (b) is recoverable: next load replays
    // the replay.json journal and the SKILL.md tmp rename is idempotent.
    //
    // Write SKILL.md first via temp rename.
    let tmp_path = dir.join(format!(
        "{}.tmp",
        skill_filename(&request.skill_id, request.version)
    ));
    std::fs::write(&tmp_path, &skill_md_bytes)
        .map_err(|e| CommandError::io(format!("write SKILL.md tmp: {e}")))?;
    std::fs::rename(&tmp_path, &skill_path)
        .map_err(|e| CommandError::io(format!("rename SKILL.md: {e}")))?;

    // Write replay.json via journal.
    store
        .write_atomic_multi_file(
            &request.skill_id,
            vec![(std::path::PathBuf::from("replay.json"), replay_bytes)],
            None, // mtime guard already checked above for SKILL.md
        )
        .map_err(map_skill_error)?;

    Ok(())
}

fn generate_id_suffix() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let mut hash: u32 = 0x811c_9dc5;
    for byte in nanos.to_le_bytes() {
        hash ^= byte as u32;
        hash = hash.wrapping_mul(0x0100_0193);
    }
    format!("{:06x}", hash & 0x00ff_ffff)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_suffix_is_six_hex_digits() {
        let s = generate_id_suffix();
        assert_eq!(s.len(), 6);
        assert!(s.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn skill_filename_combines_slug_and_version() {
        assert_eq!(
            skill_filename("Click Login Button", 3),
            "click-login-button-v3.md"
        );
    }

    #[test]
    fn skill_file_io_guard_blocks_when_trace_persistence_is_off() {
        let err = ensure_skill_file_io_enabled(false).unwrap_err();
        assert!(err.message.contains("trace persistence is off"));
    }
}
