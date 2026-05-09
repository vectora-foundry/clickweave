use super::*;

impl StateRunner {
    /// Spec 2: run an episodic-memory retrieval if the trigger conditions
    /// hold (run-start or `Recovering` entry). On `Recovering` entry,
    /// also captures the [`RecoveringEntrySnapshot`] for the eventual
    /// write at the matching `Recovering -> Executing` exit.
    ///
    /// `prev_phase_at_top` is the phase as it was at the top of the
    /// outer-loop iteration before `observe()` ran, so the
    /// `Exploring/Executing -> Recovering` transition is detectable.
    pub(crate) async fn try_retrieve_episodic(
        &mut self,
        prev_phase_at_top: crate::agent::phase::Phase,
    ) -> Vec<crate::agent::episodic::RetrievedEpisode> {
        use crate::agent::episodic::signature::compute_pre_state_signature;
        use crate::agent::episodic::{
            EpisodicStore as _, RetrievalQuery, RetrievalTrigger, RetrievedEpisode,
        };
        use crate::agent::phase::Phase;

        if !self.episodic_active() {
            return Vec::new();
        }
        let store = match &self.episodic_store {
            Some(s) => s.clone(),
            None => return Vec::new(),
        };

        // D24: run-start retrieval fires once per run, full stop.
        // `episodic_run_start_retrieved` is the authoritative gate (not
        // `step_index == 0`, which lied on synthetic-skip / policy-deny /
        // approval-reject paths because none of those
        // ticked the counter). Marked consumed on first reach so a
        // zero-hit retrieval still counts as "the run-start slot was
        // used" and can never fire a second time.
        let trigger = if !self.episodic_run_start_retrieved {
            self.episodic_run_start_retrieved = true;
            RetrievalTrigger::RunStart
        } else if prev_phase_at_top != Phase::Recovering
            && self.task_state.phase == Phase::Recovering
        {
            RetrievalTrigger::RecoveringEntry
        } else {
            return Vec::new();
        };

        let active_slots: Vec<crate::agent::task_state::WatchSlotName> =
            self.task_state.watch_slots.iter().map(|s| s.name).collect();
        let sig = compute_pre_state_signature(&self.world_model, &active_slots);

        // Capture snapshot at retrieval time so the eventual
        // write uses the same signature.
        if matches!(trigger, RetrievalTrigger::RecoveringEntry) {
            use crate::agent::episodic::types::{RecoveringEntrySnapshot, TriggeringError};
            use crate::agent::step_record::WorldModelSnapshot;
            let events_ref = self.current_events_jsonl_ref();
            let snap = WorldModelSnapshot::from_world_model(&self.world_model);
            self.recovering_snapshot = Some(RecoveringEntrySnapshot {
                entered_at_step: self.step_index,
                world_model_at_entry: snap,
                task_state_at_entry: self.task_state.clone(),
                triggering_error: TriggeringError {
                    failed_tool: self.last_failed_tool_name.clone().unwrap_or_default(),
                    error_kind: self.last_failed_error_kind.clone().unwrap_or_default(),
                    consecutive_errors_at_entry: self.consecutive_errors as u32,
                    step_index: self.step_index,
                },
                workflow_hash: self.episodic_ctx.project_id.clone(),
                pre_state_signature: sig.clone(),
                active_watch_slots: active_slots.clone(),
                events_jsonl_ref: events_ref,
            });
            self.recovery_actions_accumulator.clear();
        }

        let subgoal_owned = self.task_state.subgoal_stack.last().map(|s| s.text.clone());
        let goal_owned = self.task_state.goal.clone();
        let workflow_hash = self.episodic_ctx.project_id.clone();
        let now = chrono::Utc::now();

        let q = RetrievalQuery {
            trigger,
            pre_state_signature: &sig,
            goal: &goal_owned,
            subgoal_text: subgoal_owned.as_deref(),
            workflow_hash: &workflow_hash,
            now,
        };

        let k_each = self.config.retrieved_episodes_k.max(1) * 2;
        let mut wl_hits: Vec<RetrievedEpisode> =
            store.retrieve(&q, k_each).await.unwrap_or_default();

        let g_cap = self.config.episodic_global_cap_per_retrieval.max(1) * 2;
        let mut g_hits: Vec<RetrievedEpisode> = match &self.episodic_global {
            Some(g) => g.retrieve(&q, g_cap).await.unwrap_or_default(),
            None => Vec::new(),
        };

        for h in &mut wl_hits {
            h.score_breakdown.final_score *= self.config.episodic_workflow_priority_multiplier;
        }
        g_hits.truncate(self.config.episodic_global_cap_per_retrieval);

        let mut merged: Vec<RetrievedEpisode> = wl_hits.into_iter().chain(g_hits).collect();
        merged.sort_by(|a, b| {
            crate::agent::episodic::embedder::nan_safe_desc(
                a.score_breakdown.final_score,
                b.score_breakdown.final_score,
            )
        });
        merged.truncate(self.config.retrieved_episodes_k);

        // Emit `EpisodesRetrieved` whenever the retrieval pass returned
        // at least one candidate. Frontends use this to surface the
        // `<retrieved_recoveries>` block before the LLM call lands.
        if !merged.is_empty() {
            use crate::agent::episodic::EpisodeScope;
            let workflow_count = merged
                .iter()
                .filter(|r| matches!(r.scope, EpisodeScope::WorkflowLocal))
                .count();
            let global_count = merged.len() - workflow_count;
            let event = AgentEvent::EpisodesRetrieved {
                run_id: self.run_id,
                trigger,
                count: merged.len(),
                episode_ids: merged
                    .iter()
                    .map(|r| r.episode.episode_id.clone())
                    .collect(),
                scope_breakdown: crate::agent::types::ScopeBreakdown {
                    workflow: workflow_count,
                    global: global_count,
                },
            };
            self.emit_event(event).await;
        }

        merged
    }

    /// Apply any pending invalidation events and re-infer the phase from
    /// structural signals.
    pub fn observe(&mut self) {
        let events = std::mem::take(&mut self.pending_events);
        self.world_model.apply_events(events);
        self.task_state.phase = phase::infer(&PhaseSignals {
            stack_depth: self.task_state.subgoal_stack.len(),
            consecutive_errors: self.consecutive_errors,
            last_replan_step: self.last_replan_step,
            current_step: self.step_index,
        });
    }

    /// Apply the batch of task-state mutations from an `AgentTurn`, in
    /// order. Invalid mutations become warnings but do not abort the pass —
    /// subsequent mutations and the action still run. Matches the
    /// error-path table in the spec.
    ///
    /// PushSubgoal / CompleteSubgoal route through the per-mutation
    /// helpers on `TaskState` so the runner can capture the generated
    /// `SubgoalId` (Spec 3 retrieval hook) and the matching push-side
    /// `recorded_steps` index (Spec 3 extractor) without re-walking
    /// the mutation slice. `last_pushed_subgoal_ids` is cleared at the
    /// top of every batch — the retrieval hook reads it once per turn.
    pub fn apply_mutations(&mut self, muts: &[TaskStateMutation]) -> Vec<String> {
        let mut warnings = Vec::new();
        self.last_pushed_subgoal_ids.clear();

        for m in muts {
            match m {
                TaskStateMutation::PushSubgoal { text } => {
                    self.push_idx_stack.push(self.recorded_steps.len());
                    self.push_signature_stack.push(
                        crate::agent::skills::signature::compute_subgoal_signature(
                            text,
                            &self.world_model,
                        ),
                    );
                    let id = self.task_state.apply_push_subgoal(text, self.step_index);
                    self.last_pushed_subgoal_ids.push(id);
                    self.produced_node_ids_stack.push(Vec::new());
                }
                TaskStateMutation::CompleteSubgoal { summary } => {
                    let push_idx = self.push_idx_stack.pop().unwrap_or(0);
                    let push_sig = self.push_signature_stack.pop();
                    let produced_node_ids = self.produced_node_ids_stack.pop().unwrap_or_default();
                    match self
                        .task_state
                        .apply_complete_subgoal(summary, self.step_index)
                    {
                        Ok(milestone) => {
                            let pre_state_sig = push_sig.unwrap_or_else(|| {
                                crate::agent::skills::signature::compute_subgoal_signature(
                                    &milestone.text,
                                    &self.world_model,
                                )
                            });
                            self.completed_subgoal_extraction_queue.push((
                                push_idx,
                                milestone,
                                pre_state_sig,
                                produced_node_ids,
                            ));
                        }
                        Err(e) => warnings.push(format!("{}", e)),
                    }
                }
                other => {
                    if let Err(e) = self.task_state.apply(other, self.step_index) {
                        warnings.push(format!("{}", e));
                    }
                }
            }
        }
        warnings
    }

    pub(super) fn record_produced_node_id(&mut self, node_id: uuid::Uuid) {
        for produced_node_ids in &mut self.produced_node_ids_stack {
            produced_node_ids.push(node_id);
        }
    }

    /// Rewrite raw AX uid references in a workflow node into replay-stable
    /// `AxTarget::Descriptor` payloads using the current
    /// `last_native_ax_snapshot` body. Port of the legacy
    /// `enrich_ax_descriptor` helper — D15 moves the source of truth off
    /// the transcript onto `WorldModel`.
    ///
    /// No-op when no native AX snapshot has been captured yet, when the
    /// node type is not an AX dispatch variant, when the target is already
    /// a `Descriptor`, or when the uid is not present in the snapshot.
    pub fn enrich_ax_descriptor(&self, node_type: &mut clickweave_core::NodeType) {
        use clickweave_core::{AxTarget, NodeType};

        let Some(ax) = &self.world_model.last_native_ax_snapshot else {
            return;
        };

        let target: &mut AxTarget = match node_type {
            NodeType::AxClick(p) => &mut p.target,
            NodeType::AxSetValue(p) => &mut p.target,
            NodeType::AxSelect(p) => &mut p.target,
            _ => return,
        };

        let uid = match target {
            AxTarget::ResolvedUid(uid) if !uid.is_empty() => uid.clone(),
            _ => return,
        };

        let parsed = crate::agent::world_model::parse_ax_snapshot(&ax.value.ax_tree_text);
        let Some(entry) = parsed.into_iter().find(|e| e.uid == uid) else {
            return;
        };
        *target = AxTarget::Descriptor {
            role: entry.role,
            name: entry.name.unwrap_or_default(),
            parent_name: entry.parent_name,
        };
    }

    /// Build a workflow node for the executed tool call. Returns the UUID of
    /// the new node, or `None` when the tool is observation-only, when
    /// workflow-graph building is disabled via `config.build_workflow`, or
    /// when the tool-to-[`clickweave_core::NodeType`] mapping fails.
    ///
    /// On success the node is pushed onto `state.workflow.nodes`, an
    /// `AgentEvent::NodeAdded` fires, and — when a prior node exists —
    /// an edge from the previous node to this one is pushed onto
    /// `state.workflow.edges` with a matching `AgentEvent::EdgeAdded`. The
    /// first node in a run is chained from `state.last_node_id`, which the
    /// top-level loop seeds from the caller-provided `anchor_node_id` so the
    /// first tool call is linked to the prior workflow graph when one is
    /// supplied. Every node is stamped with `source_run_id: self.run_id`.
    ///
    /// Port of the legacy `AgentRunner::add_workflow_node`.
    pub async fn add_workflow_node(
        &mut self,
        tool_name: &str,
        arguments: &Value,
        known_tools: &[Value],
        annotations_by_tool: &HashMap<String, ToolAnnotations>,
    ) -> Option<uuid::Uuid> {
        use clickweave_core::{Node, Position, tool_mapping::tool_invocation_to_node_type};

        if !self.config.build_workflow {
            return None;
        }
        if is_observation_tool(tool_name, annotations_by_tool) {
            return None;
        }

        let mut node_type = match tool_invocation_to_node_type(tool_name, arguments, known_tools) {
            Ok(nt) => nt,
            Err(e) => {
                warn!(
                    error = %e,
                    tool = tool_name,
                    "state-spine: could not map tool to workflow node type — workflow graph will be incomplete"
                );
                self.emit_event(AgentEvent::Warning {
                    message: format!("Failed to map tool '{}' to workflow node: {}", tool_name, e),
                })
                .await;
                return None;
            }
        };

        // AX dispatch descriptor enrichment. The tool-mapping inbound path
        // writes `AxTarget::ResolvedUid(uid)`; upgrade to `Descriptor`
        // against the most recent native AX snapshot so the node replays
        // correctly after a fresh snapshot (different generation id).
        self.enrich_ax_descriptor(&mut node_type);

        let position = Position {
            x: 0.0,
            y: (self.state.workflow.nodes.len() as f32) * 120.0,
        };
        let node = Node::new(node_type, position, tool_name, "").with_run_id(self.run_id);
        let node_id = node.id;

        // Emit the live NodeAdded event before mutating the workflow so
        // subscribers observe creation order that matches the event stream.
        self.emit_event(AgentEvent::NodeAdded {
            node: Box::new(node.clone()),
        })
        .await;
        self.state.workflow.nodes.push(node);

        // Chain from the previous node (or the caller-supplied anchor on the
        // first iteration).
        if let Some(prev_id) = self.state.last_node_id {
            let edge = clickweave_core::Edge {
                from: prev_id,
                to: node_id,
            };
            self.emit_event(AgentEvent::EdgeAdded { edge: edge.clone() })
                .await;
            self.state.workflow.edges.push(edge);
        }

        self.state.last_node_id = Some(node_id);
        Some(node_id)
    }

    /// Queue invalidation events that the just-executed tool implies for
    /// the world model. Pure-observation tools (`take_ax_snapshot`,
    /// `take_screenshot`, `cdp_find_elements`, etc.) are no-ops here;
    /// state-transition tools queue the matching event so the next
    /// `observe()` call drops fields that the tool may have invalidated.
    ///
    /// Categories:
    /// - **Focus shift** (`focus_window`): drops focused-app, window list,
    ///   element surface, modal/dialog, screenshot, AX snapshot.
    /// - **App lifecycle** (`launch_app`, `quit_app`): same as focus shift.
    /// - **CDP navigation** (`cdp_navigate`, `cdp_new_page`,
    ///   `cdp_select_page`): drops the CDP page state, element surface,
    ///   and modal/dialog presence.
    ///
    /// Snapshot-staleness invalidation is event-driven from a separate
    /// top-of-loop hook (`queue_snapshot_stale_if_aged`), since it
    /// depends on the current step counter, not the tool that just ran.
    pub fn queue_invalidations_for_tool_success(&mut self, tool_name: &str, arguments: &Value) {
        if FOCUS_CHANGING_TOOLS.contains(&tool_name) {
            self.queue_invalidation(InvalidationEvent::FocusChanging {
                tool: tool_name.to_string(),
            });
        }
        if APP_LIFECYCLE_TOOLS.contains(&tool_name) {
            self.queue_invalidation(InvalidationEvent::AppLifecycle {
                tool: tool_name.to_string(),
            });
        }
        if CDP_NAVIGATION_TOOLS.contains(&tool_name) {
            let new_url = arguments
                .get("url")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            self.queue_invalidation(InvalidationEvent::CdpNavigation { new_url });
        }
    }

    /// Queue per-snapshot `SnapshotStale` events for any snapshot
    /// (`last_native_ax_snapshot` or `last_screenshot`) whose own age
    /// has crossed its `ttl_steps`. Called at the top of every loop
    /// iteration before `observe()` so the apply-events pass drops
    /// bodies that have aged out without the LLM re-capturing.
    ///
    /// One event per stale field — never a shared `age_steps` value
    /// across both fields. A fresh screenshot must not be invalidated
    /// just because the AX snapshot is stale.
    pub fn queue_snapshot_stale_if_aged(&mut self) {
        use crate::agent::world_model::SnapshotKind;
        if let Some(ax) = &self.world_model.last_native_ax_snapshot
            && let Some(ttl) = ax.ttl_steps
        {
            let age = (self.step_index.saturating_sub(ax.written_at)) as u32;
            if age > ttl {
                self.queue_invalidation(InvalidationEvent::SnapshotStale {
                    kind: SnapshotKind::NativeAx,
                    age_steps: age,
                });
            }
        }
        if let Some(ss) = &self.world_model.last_screenshot
            && let Some(ttl) = ss.ttl_steps
        {
            let age = (self.step_index.saturating_sub(ss.written_at)) as u32;
            if age > ttl {
                self.queue_invalidation(InvalidationEvent::SnapshotStale {
                    kind: SnapshotKind::Screenshot,
                    age_steps: age,
                });
            }
        }
    }

    /// After a successful tool call, refresh the world model's identity
    /// fields that the tool just captured. Non-snapshot tools are no-ops.
    pub fn update_continuity_after_tool_success(&mut self, tool_name: &str, body: &str) {
        use crate::agent::world_model::{
            AxSnapshotData, Fresh, FreshnessSource, ObservedElement, ScreenshotRef,
            parse_ax_snapshot, parse_ocr_matches,
        };
        match tool_name {
            "take_ax_snapshot" => {
                let parsed = parse_ax_snapshot(body);
                let snapshot_id = parsed
                    .first()
                    .map(|e| e.uid.clone())
                    .unwrap_or_else(|| format!("ax-{}", self.step_index));
                self.world_model.last_native_ax_snapshot = Some(Fresh {
                    value: AxSnapshotData {
                        snapshot_id,
                        element_count: parsed.len(),
                        captured_at_step: self.step_index,
                        ax_tree_text: body.to_string(),
                    },
                    written_at: self.step_index,
                    source: FreshnessSource::DirectObservation,
                    ttl_steps: Some(8),
                });
                // Mirror parsed AX elements into the source-agnostic
                // element surface so the renderer prints them alongside
                // (or instead of) CDP elements. Native-only paths
                // depend on this — without it the LLM never sees the
                // a-prefixed uid vocabulary in `<world_model>`.
                if !parsed.is_empty() {
                    let observed: Vec<ObservedElement> =
                        parsed.into_iter().map(ObservedElement::Ax).collect();
                    self.world_model.elements = Some(Fresh {
                        value: observed,
                        written_at: self.step_index,
                        source: FreshnessSource::DirectObservation,
                        ttl_steps: Some(8),
                    });
                }
            }
            "take_screenshot" => {
                let id = serde_json::from_str::<serde_json::Value>(body)
                    .ok()
                    .and_then(|v| {
                        v.get("screenshot_id")
                            .and_then(|s| s.as_str())
                            .map(String::from)
                    })
                    .unwrap_or_else(|| format!("ss-{}", self.step_index));
                self.world_model.last_screenshot = Some(Fresh {
                    value: ScreenshotRef {
                        screenshot_id: id,
                        captured_at_step: self.step_index,
                    },
                    written_at: self.step_index,
                    source: FreshnessSource::DirectObservation,
                    ttl_steps: Some(8),
                });
            }
            "find_text" => {
                // OCR results from `find_text` populate the
                // source-agnostic element surface as `ObservedElement::Ocr`
                // when the response is parseable. Parse failures are
                // tolerated silently — `find_text` has multiple legacy
                // body shapes, so a non-OCR-shaped body is normal.
                if let Ok(matches) = parse_ocr_matches(body)
                    && !matches.is_empty()
                {
                    let observed: Vec<ObservedElement> =
                        matches.into_iter().map(ObservedElement::Ocr).collect();
                    self.world_model.elements = Some(Fresh {
                        value: observed,
                        written_at: self.step_index,
                        source: FreshnessSource::DirectObservation,
                        ttl_steps: Some(2),
                    });
                }
            }
            _ => {}
        }
    }

    /// Fetch compact CDP page inventory from the current page via MCP.
    ///
    /// This deliberately calls `cdp_summarize_page`, not
    /// `cdp_find_elements`: the top-of-loop observation should tell the model
    /// which page and element categories exist without injecting a transient
    /// page-wide DOM list into every prompt. Explicit target candidates enter
    /// the transcript only when the agent asks for `cdp_find_elements`, and
    /// ambiguous matches can be expanded with `cdp_get_element_context`.
    pub(crate) async fn fetch_cdp_page_summary<M: Mcp + ?Sized>(
        &mut self,
        mcp: &M,
    ) -> CdpPageObservation {
        if !mcp.has_tool("cdp_summarize_page") {
            // No CDP surface this turn — clear the sticky URL so the
            // next-turn state-block mirror does not render a stale page.
            self.state.current_url = String::new();
            return CdpPageObservation::default();
        }
        match mcp
            .call_tool("cdp_summarize_page", Some(serde_json::json!({})))
            .await
        {
            Ok(result) if result.is_error != Some(true) => {
                let text = crate::cdp_lifecycle::extract_text(&result);
                match serde_json::from_str::<clickweave_core::cdp::CdpPageSummaryResponse>(&text) {
                    Ok(parsed) => {
                        self.state.current_url = parsed.page_url.clone();
                        let page_fingerprint = crate::agent::transition::page_inventory_fingerprint(
                            &parsed.page_url,
                            &parsed.inventory,
                        );
                        return CdpPageObservation {
                            page_url: parsed.page_url,
                            page_fingerprint,
                            inventory: parsed
                                .inventory
                                .into_iter()
                                .map(CdpElementInventorySummary::from)
                                .collect(),
                        };
                    }
                    Err(parse_err) => {
                        tracing::debug!(
                            error = %parse_err,
                            "state-spine: failed to parse cdp_summarize_page response"
                        );
                        self.emit_event(AgentEvent::Warning {
                            message: format!(
                                "cdp_summarize_page response failed to parse: {} — continuing without CDP page summary",
                                parse_err
                            ),
                        })
                        .await;
                        // Parse failure — clear the sticky URL so a later
                        // turn does not keep rendering the previous page.
                        self.state.current_url = String::new();
                    }
                }
            }
            Ok(_) => {
                // MCP returned `is_error=true` or a non-Ok result — treat
                // as "no fresh observation" and drop the sticky URL.
                self.state.current_url = String::new();
            }
            Err(e) => {
                tracing::debug!(error = %e, "state-spine: cdp_summarize_page call failed");
                self.state.current_url = String::new();
            }
        }
        CdpPageObservation::default()
    }

    /// Build a terminal `StepRecord` for a completed / halted run. Used by
    /// the control loop on run-end boundaries and by integration tests.
    pub fn build_step_record(
        &self,
        boundary_kind: crate::agent::step_record::BoundaryKind,
        action_taken: serde_json::Value,
        outcome: serde_json::Value,
    ) -> crate::agent::step_record::StepRecord {
        use crate::agent::step_record::{StepRecord, WorldModelSnapshot};
        StepRecord {
            step_index: self.step_index,
            boundary_kind,
            world_model_snapshot: WorldModelSnapshot::from_world_model(&self.world_model),
            task_state_snapshot: self.task_state.clone(),
            action_taken,
            outcome,
            timestamp: chrono::Utc::now(),
        }
    }

    /// Persist one `BoundaryKind::SubgoalCompleted` record per
    /// milestone appended during the current turn. Called from the
    /// outer loop in [`Self::run`] right after the mutation apply
    /// counts a positive `outer_milestones_appended` — before any
    /// early-exit branch (synthetic focus skip / live policy-deny /
    /// live approval-reject), so the boundary record fires whether or
    /// not the action eventually goes through `run_turn`. Records the
    /// turn's batched mutations as `action_taken` so the subgoal
    /// summaries are recoverable from `events.jsonl` without a
    /// separate transcript lookup. Emits one
    /// `AgentEvent::BoundaryRecordWritten` per persisted record.
    pub(super) async fn write_subgoal_completed_records(&mut self, count: usize, turn: &AgentTurn) {
        let action_taken =
            serde_json::to_value(&turn.mutations).unwrap_or_else(|_| serde_json::json!([]));
        let milestone_start = self.task_state.milestones.len().saturating_sub(count);
        for i in 0..count {
            let milestone_text = self
                .task_state
                .milestones
                .get(milestone_start + i)
                .map(|m| m.text.clone());
            self.persist_boundary_record(
                crate::agent::step_record::BoundaryKind::SubgoalCompleted,
                action_taken.clone(),
                serde_json::json!({"kind": "subgoal_completed"}),
                milestone_text,
            )
            .await;
        }

        // Spec 3: drain the extraction queue populated by
        // `apply_mutations`. Each completed-subgoal milestone has both
        // its push-side `recorded_steps` index, the milestone payload,
        // and the node lineage for that subgoal frame available without
        // re-walking `task_state.milestones`.
        let queue = std::mem::take(&mut self.completed_subgoal_extraction_queue);
        if !queue.is_empty() && self.skill_ctx.enabled && self.config.skills_enabled {
            let workflow_hash = self.episodic_ctx.project_id.clone();
            let run_id = self.run_id;
            let step_index = self.state.steps.len();

            for (push_idx, milestone, pre_state_sig, produced_node_ids) in queue {
                let action_sequence = if push_idx < self.recorded_steps.len() {
                    self.recorded_steps[push_idx..].to_vec()
                } else {
                    Vec::new()
                };
                match crate::agent::skills::extractor::maybe_extract_skill(
                    &milestone,
                    &action_sequence,
                    pre_state_sig,
                    &self.world_model,
                    &self.skill_index,
                    &self.skill_store,
                    &self.skill_ctx,
                    run_id,
                    &workflow_hash,
                    step_index,
                    &produced_node_ids,
                )
                .await
                {
                    Ok(crate::agent::skills::MaybeExtracted::Inserted {
                        skill_id,
                        version,
                        ..
                    })
                    | Ok(crate::agent::skills::MaybeExtracted::Merged {
                        skill_id, version, ..
                    }) => {
                        let (state, scope) = self
                            .skill_index
                            .read()
                            .get(&skill_id, version)
                            .map(|s| (s.state, s.scope))
                            .unwrap_or((
                                crate::agent::skills::SkillState::Draft,
                                crate::agent::skills::SkillScope::ProjectLocal,
                            ));
                        self.emit_event(AgentEvent::SkillExtracted {
                            run_id: self.run_id,
                            skill_id,
                            version,
                            state,
                            scope,
                        })
                        .await;
                    }
                    Ok(_) => {}
                    Err(err) => {
                        tracing::warn!(?err, "skills: extraction failed; continuing");
                    }
                }
            }
        }
    }

    /// Persist one `BoundaryKind::RecoverySucceeded` record on the exact
    /// `Recovering -> Executing` transition (D8). Called from
    /// [`Self::run`] when a tool success cleared the consecutive-error
    /// streak. `action_taken` / `outcome` record the successful turn so
    /// Spec 2's episodic memory can reason about what resolved the
    /// recovery. Emits one `AgentEvent::BoundaryRecordWritten` (D17).
    pub(super) async fn write_recovery_succeeded_record(
        &self,
        turn: &AgentTurn,
        outcome: &TurnOutcome,
    ) {
        let action_taken =
            serde_json::to_value(&turn.action).unwrap_or_else(|_| serde_json::json!({}));
        let outcome_json = match outcome {
            TurnOutcome::ToolSuccess {
                tool_name,
                tool_body,
            } => serde_json::json!({
                "kind": "tool_success",
                "tool_name": tool_name,
                "body_len": tool_body.len(),
            }),
            // RecoverySucceeded is only written on ToolSuccess; the other
            // variants never reach this path (see `run()`'s guard).
            _ => serde_json::json!({"kind": "tool_success"}),
        };
        self.persist_boundary_record(
            crate::agent::step_record::BoundaryKind::RecoverySucceeded,
            action_taken,
            outcome_json,
            None,
        )
        .await;
    }

    /// Persist the single `BoundaryKind::Terminal` record at run end (D8).
    /// Called exactly once from [`Self::run`] after the control loop has
    /// populated `state.terminal_reason`. Encodes the terminal reason into
    /// the outcome payload so the record is self-describing without a
    /// cross-reference to the rest of `events.jsonl`. Emits one
    /// `AgentEvent::BoundaryRecordWritten` (D17).
    pub(super) async fn write_terminal_record(&self) {
        let terminal_reason = self.state.terminal_reason.as_ref();
        let outcome_json = terminal_reason
            .map(|tr| serde_json::to_value(tr).unwrap_or_else(|_| serde_json::json!({})))
            .unwrap_or_else(|| serde_json::json!({"kind": "unknown"}));
        // Best-effort action_taken: a minimal projection of the last
        // recorded step (tool_name only — `AgentCommand` itself isn't
        // `Serialize`). Falls back to the outcome for zero-step runs.
        let action_taken = self
            .state
            .steps
            .last()
            .map(|step| {
                serde_json::json!({
                    "tool_name": step.command.tool_name_or_unknown(),
                    "step_index": step.index,
                })
            })
            .unwrap_or_else(|| outcome_json.clone());
        self.persist_boundary_record(
            crate::agent::step_record::BoundaryKind::Terminal,
            action_taken,
            outcome_json,
            None,
        )
        .await;
    }

    /// Shared body for the three `write_*_record` boundary paths: build
    /// the `StepRecord`, persist via `RunStorage`, and emit the matching
    /// `BoundaryRecordWritten` event.
    async fn persist_boundary_record(
        &self,
        boundary_kind: crate::agent::step_record::BoundaryKind,
        action_taken: serde_json::Value,
        outcome: serde_json::Value,
        milestone_text: Option<String>,
    ) {
        let record = self.build_step_record(boundary_kind.clone(), action_taken, outcome);
        self.write_step_record(&record);
        self.emit_event(AgentEvent::BoundaryRecordWritten {
            run_id: self.run_id,
            boundary_kind,
            step_index: record.step_index,
            milestone_text,
        })
        .await;
    }
}
