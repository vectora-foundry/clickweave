use super::*;

impl StateRunner {
    fn start_skill_watcher_if_enabled(&mut self) {
        if !self.skill_ctx.enabled
            || !self.config.skills_enabled
            || self.skill_watcher_handle.is_some()
        {
            return;
        }

        let mut dirs = Vec::new();
        let mut stores = Vec::new();

        let project_dir = self.skill_ctx.project_skills_dir.clone();
        if let Err(err) = std::fs::create_dir_all(&project_dir) {
            warn!(
                ?project_dir,
                ?err,
                "skills: failed to create project skills dir for watcher"
            );
            return;
        }
        dirs.push(project_dir);
        stores.push(self.skill_store.clone());

        if let Some(global_dir) = self.skill_ctx.global_skills_dir.clone() {
            if let Err(err) = std::fs::create_dir_all(&global_dir) {
                warn!(
                    ?global_dir,
                    ?err,
                    "skills: failed to create global skills dir for watcher"
                );
            } else {
                dirs.push(global_dir.clone());
                stores.push(Arc::new(SkillStore::new(global_dir)));
            }
        }

        match crate::agent::skills::watcher::SkillWatcher::spawn(dirs) {
            Ok(watcher) => {
                self.skill_watcher_handle = Some(
                    crate::agent::skills::watcher_consumer::WatcherConsumer::spawn_watcher(
                        self.skill_index.clone(),
                        stores,
                        watcher,
                    ),
                );
            }
            Err(err) => {
                warn!(
                    ?err,
                    "skills: watcher failed to start; external edits will be picked up on next run"
                );
            }
        }
    }

    fn initialize_run_loop(
        &mut self,
        goal: &str,
        workflow: clickweave_core::Workflow,
        mcp_tools: &[Value],
        anchor_node_id: Option<uuid::Uuid>,
    ) -> RunLoopContext {
        // Reset the visible state tuple to match the freshly-provided
        // workflow. `AgentState::new(workflow)` wipes steps/terminal_reason
        // so the same `StateRunner` could in theory be reused across runs,
        // though `self` is consumed by the public run wrapper.
        self.state = AgentState::new(workflow);
        self.state.last_node_id = anchor_node_id;

        // Build the system prompt from the raw openai-shaped tool list.
        // `build_system_prompt` expects `clickweave_mcp::Tool`; the raw
        // `Vec<Value>` is already openai-shape, so extract the minimum
        // fields each tool entry carries.
        //
        // D18: the system prompt is stable across runs. Variant context +
        // prior-turn log are pre-composed into `goal` at the caller seam, so
        // they land in `messages[1]`, preserving the `messages[0]` cache prefix.
        let tool_list_for_prompt = openai_tools_to_mcp_tool_list(mcp_tools);
        let system_text = if let Some(prompt) = self.agent_system_prompt_override.as_deref() {
            build_system_prompt_with_header(prompt, &tool_list_for_prompt)
        } else {
            build_system_prompt(&tool_list_for_prompt)
        };

        let advertised_tool_names: Vec<String> = tool_list_for_prompt
            .iter()
            .map(|t| t.name.clone())
            .collect();

        let initial_scope = self.compute_tools_in_scope(&advertised_tool_names);
        let initial_user = build_user_turn_message_from_input(UserTurnMessageInput {
            wm: &self.world_model,
            ts: &self.task_state,
            current_step: 0,
            observation_text: goal,
            retrieved: &[],
            applicable_skills: &[],
            tools_in_scope_names: &initial_scope,
            max_elements: self.config.state_block_max_elements,
        });

        RunLoopContext {
            messages: vec![Message::system(system_text), Message::user(initial_user)],
            tools: mcp_tools
                .iter()
                .cloned()
                .chain(crate::agent::prompt::pseudo_tools())
                .collect(),
            advertised_tool_names,
            annotations_by_tool: build_annotations_index(mcp_tools),
            budget: CompactBudget {
                recent_n: self.config.recent_n,
                ..CompactBudget::default()
            },
        }
    }

    async fn observe_for_next_turn<M>(
        &mut self,
        mcp: &M,
    ) -> (
        Vec<clickweave_core::cdp::CdpFindElementMatch>,
        Vec<crate::agent::episodic::RetrievedEpisode>,
    )
    where
        M: Mcp + ?Sized,
    {
        // Capture the pre-mirror world-model signatures so the
        // `WorldModelChanged` diff emitted by `run_turn` sees the
        // direct-observation writes below. Only seed the baseline when it is
        // empty: early-exit branches skip `run_turn`, so the baseline must
        // persist across iterations until `run_turn.take()` consumes it.
        if self.turn_pre_signatures.is_none() {
            self.turn_pre_signatures = Some(self.world_model.field_signatures());
        }
        // Spec 3: snapshot the world model before this iteration's dispatch
        // so successful tool calls record the state the LLM actually saw.
        self.pre_dispatch_snapshot = Some(
            crate::agent::step_record::WorldModelSnapshot::from_world_model(&self.world_model),
        );

        let CdpPageObservation {
            page_url,
            page_fingerprint,
            inventory,
        } = self.fetch_cdp_page_summary(mcp).await;
        self.mirror_cdp_page_summary(page_url, page_fingerprint, inventory);

        let elements = self.current_cdp_elements();
        let prev_phase_at_top = self.task_state.phase;
        self.queue_snapshot_stale_if_aged();
        self.observe();
        if prev_phase_at_top != self.task_state.phase {
            self.emit_event(AgentEvent::TaskStateChanged {
                run_id: self.run_id,
                task_state: self.task_state.clone(),
            })
            .await;
        }

        let retrieved = self.try_retrieve_episodic(prev_phase_at_top).await;
        (elements, retrieved)
    }

    fn mirror_cdp_page_summary(
        &mut self,
        page_url: String,
        page_fingerprint: String,
        inventory: Vec<CdpElementInventorySummary>,
    ) {
        use crate::agent::world_model::{CdpPageState, Fresh, FreshnessSource, ObservedElement};

        if matches!(
            self.world_model
                .elements
                .as_ref()
                .and_then(|f| f.value.first()),
            Some(ObservedElement::Cdp(_))
        ) {
            self.world_model.elements = None;
        }

        let url = if page_url.is_empty() {
            self.state.current_url.clone()
        } else {
            page_url
        };
        if !url.is_empty() {
            self.world_model.cdp_page = Some(Fresh {
                value: CdpPageState {
                    url,
                    page_fingerprint,
                    element_inventory: inventory,
                },
                written_at: self.step_index,
                source: FreshnessSource::DirectObservation,
                ttl_steps: Some(2),
            });
        } else {
            self.world_model.cdp_page = None;
        }
    }

    fn current_cdp_elements(&self) -> Vec<clickweave_core::cdp::CdpFindElementMatch> {
        self.world_model
            .elements
            .as_ref()
            .map(|fresh| {
                fresh
                    .value
                    .iter()
                    .filter_map(|element| match element {
                        crate::agent::world_model::ObservedElement::Cdp(match_) => {
                            Some(match_.clone())
                        }
                        _ => None,
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    async fn prepare_turn_for_dispatch(
        &mut self,
        turn: &mut AgentTurn,
        last_action: &mut Option<LastActionProgress>,
        recent_actions: &mut VecDeque<ActionProgressSignature>,
    ) {
        let outer_milestones_before = self.task_state.milestones.len();
        if !turn.mutations.is_empty() {
            let warnings = self.apply_mutations(&turn.mutations);
            for w in warnings {
                tracing::warn!(warning = %w, "state-spine: mutation warning");
            }
            self.emit_event(AgentEvent::TaskStateChanged {
                run_id: self.run_id,
                task_state: self.task_state.clone(),
            })
            .await;
        }
        let outer_milestones_appended = self
            .task_state
            .milestones
            .len()
            .saturating_sub(outer_milestones_before);

        if outer_milestones_appended > 0 {
            self.write_subgoal_completed_records(outer_milestones_appended, turn)
                .await;
            reset_no_progress_tracking(last_action, recent_actions);
        }

        self.retrieve_skills_for_pushed_subgoals();

        if let AgentAction::InvokeSkill {
            skill_id,
            version,
            parameters,
        } = turn.action.clone()
        {
            turn.action = match self.dispatch_skill(&skill_id, version, parameters).await {
                Ok(frame) => Self::skill_frame_to_single_step_action(&frame),
                Err(reason) => AgentAction::AgentReplan { reason },
            };
        }
    }

    fn retrieve_skills_for_pushed_subgoals(&mut self) {
        if !self.skill_ctx.enabled
            || !self.config.skills_enabled
            || self.last_pushed_subgoal_ids.is_empty()
        {
            return;
        }

        let pushed = std::mem::take(&mut self.last_pushed_subgoal_ids);
        let k = self.config.applicable_skills_k;
        for id in &pushed {
            let Some(subgoal) = self
                .task_state
                .subgoal_stack
                .iter()
                .find(|s| s.id == *id)
                .cloned()
            else {
                continue;
            };
            let subgoal_sig = crate::agent::skills::signature::compute_subgoal_signature(
                &subgoal.text,
                &self.world_model,
            );
            let app_sig =
                crate::agent::skills::signature::compute_applicability_signature(&self.world_model);
            let candidates = self.skill_index.read().lookup_at(
                &subgoal_sig,
                &app_sig,
                &subgoal.text,
                k,
                chrono::Utc::now(),
            );
            self.pending_applicable_skills.extend(candidates);
        }
    }

    fn push_tool_step(
        &mut self,
        elements: &[CdpFindElementMatch],
        tool_name: &str,
        arguments: &Value,
        tool_call_id: &str,
        outcome: StepOutcome,
    ) -> usize {
        let step_idx = self.state.steps.len();
        self.state.steps.push(AgentStep {
            index: step_idx,
            elements: elements.to_vec(),
            command: AgentCommand::ToolCall {
                tool_name: tool_name.to_string(),
                arguments: arguments.clone(),
                tool_call_id: tool_call_id.to_string(),
            },
            outcome,
            page_url: self.state.current_url.clone(),
        });
        self.advance_recorded_step_index();
        step_idx
    }

    fn clear_success_dispatch_state(&mut self, trackers: &mut RunLoopTrackers) {
        self.state.consecutive_errors = 0;
        self.consecutive_errors = 0;
        trackers.last_failure = None;
        self.clear_last_failure_tracking();
    }

    async fn finish_synthetic_success(
        &mut self,
        loop_ctx: &mut RunLoopContext,
        trackers: &mut RunLoopTrackers,
        step_idx: usize,
        tool_name: &str,
        arguments: &Value,
        tool_call_id: &str,
        body: &str,
    ) {
        trackers.previous_result = Some(body.to_string());
        if let Some(nudge) = self
            .track_repeat_action(
                tool_name,
                arguments,
                body,
                &loop_ctx.annotations_by_tool,
                &mut trackers.last_action,
                &mut trackers.recent_actions,
            )
            .await
        {
            trackers.previous_result = Some(nudge);
        }
        self.emit_event(AgentEvent::StepCompleted {
            step_index: step_idx,
            tool_name: tool_name.to_string(),
            summary: crate::agent::prompt::truncate_summary(body, 120),
        })
        .await;
        append_assistant_and_tool_result(
            &mut loop_ctx.messages,
            tool_name,
            arguments,
            tool_call_id,
            trackers.previous_result.as_deref(),
        );
    }

    async fn handle_no_focus_launch_skip<M>(
        &mut self,
        turn: &AgentTurn,
        elements: &[CdpFindElementMatch],
        loop_ctx: &mut RunLoopContext,
        trackers: &mut RunLoopTrackers,
        mcp: &M,
    ) -> bool
    where
        M: Mcp + ?Sized,
    {
        let AgentAction::ToolCall {
            tool_name,
            arguments,
            tool_call_id,
        } = &turn.action
        else {
            return false;
        };
        if tool_name != "launch_app" {
            return false;
        }
        let Some(running) = self.running_app_for_no_focus_launch(arguments, mcp).await else {
            return false;
        };

        self.emit_event(AgentEvent::SubAction {
            tool_name: "launch_app".to_string(),
            summary: "skipped: app already running; focus changes disabled".to_string(),
        })
        .await;
        let skip_body = Self::skipped_launch_result_text(&running);
        debug!(
            tool = "launch_app",
            app = running.name,
            "state-spine: suppressing launch_app for already-running app",
        );
        let step_idx = self.push_tool_step(
            elements,
            tool_name,
            arguments,
            tool_call_id,
            StepOutcome::Success(skip_body.clone()),
        );
        self.emit_world_model_changed_for_recorded_step().await;
        self.clear_success_dispatch_state(trackers);
        self.maybe_cdp_connect(tool_name, arguments, &skip_body, mcp)
            .await;
        self.finish_synthetic_success(
            loop_ctx,
            trackers,
            step_idx,
            tool_name,
            arguments,
            tool_call_id,
            &skip_body,
        )
        .await;
        true
    }

    async fn handle_synthetic_focus_skip<M>(
        &mut self,
        turn: &AgentTurn,
        elements: &[CdpFindElementMatch],
        loop_ctx: &mut RunLoopContext,
        trackers: &mut RunLoopTrackers,
        mcp: &M,
    ) -> bool
    where
        M: Mcp + ?Sized,
    {
        let AgentAction::ToolCall {
            tool_name,
            arguments,
            tool_call_id,
        } = &turn.action
        else {
            return false;
        };
        if tool_name != "focus_window" {
            return false;
        }
        let Some(reason) = self.should_skip_focus_window(arguments, mcp) else {
            return false;
        };

        self.emit_event(AgentEvent::SubAction {
            tool_name: "focus_window".to_string(),
            summary: reason.sub_action_summary().to_string(),
        })
        .await;
        let skip_body = reason.llm_message().to_string();
        debug!(
            tool = "focus_window",
            app = arguments
                .get("app_name")
                .and_then(|v| v.as_str())
                .unwrap_or(""),
            reason = skip_body,
            "state-spine: suppressing focus_window",
        );
        let step_idx = self.push_tool_step(
            elements,
            tool_name,
            arguments,
            tool_call_id,
            StepOutcome::Success(skip_body.clone()),
        );
        self.emit_world_model_changed_for_recorded_step().await;
        self.clear_success_dispatch_state(trackers);

        if let Some((app_name, kind_hint)) =
            self.cdp_target_for_skipped_focus_window(reason, arguments, mcp)
            && let Some(cdp_port) = self
                .auto_connect_cdp(&app_name, kind_hint.as_deref(), mcp)
                .await
        {
            self.finalize_cdp_connected(&app_name, cdp_port, mcp).await;
        }

        self.finish_synthetic_success(
            loop_ctx,
            trackers,
            step_idx,
            tool_name,
            arguments,
            tool_call_id,
            &skip_body,
        )
        .await;
        true
    }

    async fn record_blocked_tool_error(
        &mut self,
        loop_ctx: &mut RunLoopContext,
        trackers: &mut RunLoopTrackers,
        elements: &[CdpFindElementMatch],
        tool_name: &str,
        arguments: &Value,
        tool_call_id: &str,
        err_msg: String,
        sub_action_summary: &str,
        record_policy_deny: bool,
    ) -> LoopStepFlow {
        self.emit_event(AgentEvent::SubAction {
            tool_name: tool_name.to_string(),
            summary: sub_action_summary.to_string(),
        })
        .await;
        let step_idx = self.push_tool_step(
            elements,
            tool_name,
            arguments,
            tool_call_id,
            StepOutcome::Error(err_msg.clone()),
        );
        self.emit_world_model_changed_for_recorded_step().await;
        if record_policy_deny {
            self.record_policy_deny_failure(tool_name);
        }
        self.state.consecutive_errors += 1;
        self.consecutive_errors = self.state.consecutive_errors;
        trackers.previous_result = Some(err_msg.clone());
        append_assistant_and_tool_result(
            &mut loop_ctx.messages,
            tool_name,
            arguments,
            tool_call_id,
            trackers.previous_result.as_deref(),
        );
        self.emit_event(AgentEvent::StepFailed {
            step_index: step_idx,
            tool_name: tool_name.to_string(),
            error: err_msg.clone(),
        })
        .await;

        let looped = matches!(
            trackers.last_failure.as_ref(),
            Some((prev_tool, prev_args, prev_err))
                if prev_tool == tool_name && prev_args == arguments && prev_err == &err_msg
        );
        if looped {
            warn!(
                tool = %tool_name,
                "state-spine: identical blocked tool call repeated — aborting"
            );
            self.state.terminal_reason = Some(TerminalReason::LoopDetected {
                tool_name: tool_name.to_string(),
                error: err_msg,
            });
            return LoopStepFlow::Break;
        }
        trackers.last_failure = Some((tool_name.to_string(), arguments.clone(), err_msg));

        let action = recovery_strategy(
            self.state.consecutive_errors,
            self.config.max_consecutive_errors,
        );
        if matches!(action, RecoveryAction::Abort) {
            warn!(
                errors = self.state.consecutive_errors,
                "state-spine: too many consecutive blocked tool calls — aborting"
            );
            self.state.terminal_reason = Some(TerminalReason::MaxErrorsReached {
                consecutive_errors: self.state.consecutive_errors,
            });
            return LoopStepFlow::Break;
        }
        reset_no_progress_tracking(&mut trackers.last_action, &mut trackers.recent_actions);
        LoopStepFlow::Continue
    }

    async fn guard_runtime_managed_cdp(
        &mut self,
        turn: &AgentTurn,
        elements: &[CdpFindElementMatch],
        loop_ctx: &mut RunLoopContext,
        trackers: &mut RunLoopTrackers,
    ) -> LoopStepFlow {
        let AgentAction::ToolCall {
            tool_name,
            arguments,
            tool_call_id,
        } = &turn.action
        else {
            return LoopStepFlow::Dispatch;
        };
        let Some(err_msg) = Self::raw_cdp_lifecycle_blocked(tool_name, arguments) else {
            return LoopStepFlow::Dispatch;
        };
        warn!(
            tool = %tool_name,
            "state-spine: raw CDP lifecycle tool blocked"
        );
        self.record_blocked_tool_error(
            loop_ctx,
            trackers,
            elements,
            tool_name,
            arguments,
            tool_call_id,
            err_msg,
            "blocked: CDP lifecycle is runtime-managed",
            false,
        )
        .await
    }

    async fn guard_coordinate_primitive<M>(
        &mut self,
        turn: &AgentTurn,
        elements: &[CdpFindElementMatch],
        loop_ctx: &mut RunLoopContext,
        trackers: &mut RunLoopTrackers,
        mcp: &M,
    ) -> LoopStepFlow
    where
        M: Mcp + ?Sized,
    {
        let AgentAction::ToolCall {
            tool_name,
            arguments,
            tool_call_id,
        } = &turn.action
        else {
            return LoopStepFlow::Dispatch;
        };
        let Some(err_msg) = self.coordinate_primitive_blocked(tool_name, mcp) else {
            return LoopStepFlow::Dispatch;
        };
        warn!(
            tool = %tool_name,
            "state-spine: coordinate primitive blocked by structured-surface guard"
        );
        self.record_blocked_tool_error(
            loop_ctx,
            trackers,
            elements,
            tool_name,
            arguments,
            tool_call_id,
            err_msg,
            "blocked: structured surface wired (CDP/AX)",
            false,
        )
        .await
    }

    async fn handle_permission_gate(
        &mut self,
        turn: &AgentTurn,
        elements: &[CdpFindElementMatch],
        loop_ctx: &mut RunLoopContext,
        trackers: &mut RunLoopTrackers,
    ) -> LoopStepFlow {
        let AgentAction::ToolCall {
            tool_name,
            arguments,
            tool_call_id,
        } = &turn.action
        else {
            return LoopStepFlow::Dispatch;
        };
        if is_observation_tool(tool_name, &loop_ctx.annotations_by_tool) {
            return LoopStepFlow::Dispatch;
        }

        match self.policy_for(tool_name, arguments, &loop_ctx.annotations_by_tool) {
            PermissionAction::Deny => {
                warn!(tool = %tool_name, "state-spine: tool denied by permission policy");
                self.record_blocked_tool_error(
                    loop_ctx,
                    trackers,
                    elements,
                    tool_name,
                    arguments,
                    tool_call_id,
                    format!("Tool `{}` denied by permission policy", tool_name),
                    "blocked: permission policy denied tool",
                    true,
                )
                .await
            }
            PermissionAction::Allow => {
                debug!(
                    tool = %tool_name,
                    "state-spine: permission policy allowed tool — skipping approval"
                );
                LoopStepFlow::Dispatch
            }
            PermissionAction::Ask => {
                match self
                    .request_approval(tool_name, arguments, self.state.steps.len(), "")
                    .await
                {
                    Some(ApprovalResult::Rejected) => {
                        self.record_approval_rejection(
                            loop_ctx,
                            trackers,
                            elements,
                            tool_name,
                            arguments,
                            tool_call_id,
                        )
                        .await;
                        LoopStepFlow::Continue
                    }
                    Some(ApprovalResult::Unavailable) => {
                        warn!("state-spine: approval system unavailable — terminating");
                        self.state.terminal_reason = Some(TerminalReason::ApprovalUnavailable);
                        LoopStepFlow::Break
                    }
                    Some(ApprovalResult::Approved) | None => LoopStepFlow::Dispatch,
                }
            }
        }
    }

    async fn record_approval_rejection(
        &mut self,
        loop_ctx: &mut RunLoopContext,
        trackers: &mut RunLoopTrackers,
        elements: &[CdpFindElementMatch],
        tool_name: &str,
        arguments: &Value,
        tool_call_id: &str,
    ) {
        let step_idx = self.push_tool_step(
            elements,
            tool_name,
            arguments,
            tool_call_id,
            StepOutcome::Replan("User rejected action".to_string()),
        );
        self.emit_world_model_changed_for_recorded_step().await;
        trackers.previous_result = Some("Replan: user rejected action".to_string());
        append_assistant_and_tool_result(
            &mut loop_ctx.messages,
            tool_name,
            arguments,
            tool_call_id,
            trackers.previous_result.as_deref(),
        );
        let _ = step_idx;
        reset_no_progress_tracking(&mut trackers.last_action, &mut trackers.recent_actions);
    }

    async fn handle_run_turn_result<M>(
        &mut self,
        goal: &str,
        mcp: &M,
        mcp_tools: &[Value],
        loop_ctx: &mut RunLoopContext,
        trackers: &mut RunLoopTrackers,
        turn: &AgentTurn,
        elements: &[CdpFindElementMatch],
        previous_errors: usize,
        outcome: TurnOutcome,
    ) -> LoopStepFlow
    where
        M: Mcp + ?Sized,
    {
        self.record_episodic_progress(turn, &outcome);
        self.queue_recovery_success_write(turn, &outcome, previous_errors)
            .await;

        let flow = match outcome {
            TurnOutcome::ToolSuccess {
                tool_name,
                tool_body,
            } => {
                self.handle_tool_success_outcome(
                    mcp, mcp_tools, loop_ctx, trackers, turn, elements, tool_name, tool_body,
                )
                .await
            }
            TurnOutcome::ToolError { tool_name, error } => {
                self.handle_tool_error_outcome(trackers, turn, elements, tool_name, error)
                    .await
            }
            TurnOutcome::Done { summary } => self.handle_done_outcome(goal, mcp, summary).await,
            TurnOutcome::Replan { reason } => {
                trackers.previous_result = Some(format!("replan: {}", reason));
                reset_no_progress_tracking(&mut trackers.last_action, &mut trackers.recent_actions);
                LoopStepFlow::Continue
            }
        };

        if matches!(flow, LoopStepFlow::Continue) {
            self.append_action_result_to_history(loop_ctx, trackers, &turn.action);
        }
        flow
    }

    fn record_episodic_progress(&mut self, turn: &AgentTurn, outcome: &TurnOutcome) {
        if !self.episodic_active() {
            return;
        }
        match outcome {
            TurnOutcome::ToolError { tool_name, error } => {
                self.last_failed_tool_name = Some(tool_name.clone());
                self.last_failed_error_kind = Some(error.clone());
            }
            TurnOutcome::ToolSuccess { .. } => {
                self.clear_last_failure_tracking();
            }
            _ => {}
        }
        if self.task_state.phase == crate::agent::phase::Phase::Recovering
            && let AgentAction::ToolCall {
                tool_name,
                arguments,
                ..
            } = &turn.action
        {
            let outcome_kind = match outcome {
                TurnOutcome::ToolSuccess { .. } => "ok",
                TurnOutcome::ToolError { .. } => "error",
                TurnOutcome::Done { .. } => "done",
                TurnOutcome::Replan { .. } => "replan",
            };
            self.recovery_actions_accumulator
                .push(crate::agent::episodic::types::CompactAction {
                    tool_name: tool_name.clone(),
                    brief_args: brief_summarize_args(arguments),
                    outcome_kind: outcome_kind.to_string(),
                });
        }
    }

    async fn queue_recovery_success_write(
        &mut self,
        turn: &AgentTurn,
        outcome: &TurnOutcome,
        previous_errors: usize,
    ) {
        if previous_errors == 0
            || self.consecutive_errors != 0
            || !matches!(outcome, TurnOutcome::ToolSuccess { .. })
        {
            return;
        }

        self.write_recovery_succeeded_record(turn, outcome).await;
        if !self.episodic_active() {
            return;
        }
        let Some(entry) = self.recovering_snapshot.take() else {
            return;
        };
        let Some(writer) = &self.episodic_writer else {
            return;
        };
        let actions = std::mem::take(&mut self.recovery_actions_accumulator);
        let record = self.build_step_record(
            crate::agent::step_record::BoundaryKind::RecoverySucceeded,
            serde_json::to_value(&turn.action).unwrap_or_else(|_| serde_json::json!({})),
            serde_json::json!({"kind": "tool_success"}),
        );
        let queue_result = writer
            .queue(
                crate::agent::episodic::types::WriteRequest::DeriveAndInsert {
                    entry: Box::new(entry),
                    recovery_success: Box::new(record),
                    recovery_actions: actions,
                },
            )
            .await;
        if let Err(e) = queue_result {
            self.emit_event(AgentEvent::Warning {
                message: format!("episodic: write dropped: backpressure ({e})"),
            })
            .await;
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn handle_tool_success_outcome<M>(
        &mut self,
        mcp: &M,
        mcp_tools: &[Value],
        loop_ctx: &RunLoopContext,
        trackers: &mut RunLoopTrackers,
        turn: &AgentTurn,
        elements: &[CdpFindElementMatch],
        tool_name: String,
        tool_body: String,
    ) -> LoopStepFlow
    where
        M: Mcp + ?Sized,
    {
        let AgentAction::ToolCall {
            arguments,
            tool_call_id,
            ..
        } = &turn.action
        else {
            unreachable!("ToolSuccess outcome implies ToolCall action");
        };

        let step_idx = self.push_tool_step(
            elements,
            &tool_name,
            arguments,
            tool_call_id,
            StepOutcome::Success(tool_body.clone()),
        );
        let unverified_side_effect =
            is_unverified_side_effect_action(&tool_name, arguments, &loop_ctx.annotations_by_tool);
        self.recorded_steps.push(RecordedStep {
            tool_name: tool_name.clone(),
            arguments: arguments.clone(),
            result_text: tool_body.clone(),
            world_model_pre: self.pre_dispatch_snapshot.take().unwrap_or_else(|| {
                crate::agent::step_record::WorldModelSnapshot::from_world_model(&self.world_model)
            }),
            world_model_post: crate::agent::step_record::WorldModelSnapshot::from_world_model(
                &self.world_model,
            ),
        });
        let unverified_side_effect_nudge = if unverified_side_effect {
            Some(build_unverified_side_effect_nudge(&tool_body))
        } else {
            None
        };
        trackers.previous_result = Some(
            unverified_side_effect_nudge
                .clone()
                .unwrap_or(tool_body.clone()),
        );
        trackers.last_failure = None;

        self.emit_event(AgentEvent::StepCompleted {
            step_index: step_idx,
            tool_name: tool_name.clone(),
            summary: crate::agent::prompt::truncate_summary(&tool_body, 120),
        })
        .await;
        if unverified_side_effect {
            self.emit_event(AgentEvent::Warning {
                message: format!(
                    "{}: `{}` result requires verification before completion",
                    UNVERIFIED_SIDE_EFFECT_PREFIX, tool_name
                ),
            })
            .await;
        }
        if matches!(
            self.maybe_halt_on_destructive_cap(&tool_name, &loop_ctx.annotations_by_tool),
            CapStatus::CapReached
        ) {
            self.emit_destructive_cap_hit().await;
            return LoopStepFlow::Break;
        }

        if let Some(node_id) = self
            .add_workflow_node(
                &tool_name,
                arguments,
                mcp_tools,
                &loop_ctx.annotations_by_tool,
            )
            .await
        {
            self.record_produced_node_id(node_id);
        }
        self.maybe_cdp_connect(&tool_name, arguments, &tool_body, mcp)
            .await;

        if let Some(nudge) = self
            .track_post_text_submit_search(
                &tool_name,
                arguments,
                &tool_body,
                &mut trackers.pending_text_submit_search,
            )
            .await
        {
            trackers.previous_result = Some(combine_with_side_effect_nudge(
                unverified_side_effect_nudge.as_deref(),
                nudge,
            ));
        }
        if let Some(nudge) = self
            .track_repeat_action(
                &tool_name,
                arguments,
                &tool_body,
                &loop_ctx.annotations_by_tool,
                &mut trackers.last_action,
                &mut trackers.recent_actions,
            )
            .await
        {
            trackers.previous_result = Some(combine_with_side_effect_nudge(
                unverified_side_effect_nudge.as_deref(),
                nudge,
            ));
        }
        LoopStepFlow::Continue
    }

    async fn handle_tool_error_outcome(
        &mut self,
        trackers: &mut RunLoopTrackers,
        turn: &AgentTurn,
        elements: &[CdpFindElementMatch],
        tool_name: String,
        error: String,
    ) -> LoopStepFlow {
        let AgentAction::ToolCall {
            arguments,
            tool_call_id,
            ..
        } = &turn.action
        else {
            unreachable!("ToolError outcome implies ToolCall action");
        };
        let step_idx = self.push_tool_step(
            elements,
            &tool_name,
            arguments,
            tool_call_id,
            StepOutcome::Error(error.clone()),
        );
        self.state.consecutive_errors = self.consecutive_errors;
        trackers.previous_result = Some(error.clone());
        self.emit_event(AgentEvent::StepFailed {
            step_index: step_idx,
            tool_name: tool_name.clone(),
            error: error.clone(),
        })
        .await;

        let looped = matches!(
            trackers.last_failure.as_ref(),
            Some((prev_tool, prev_args, prev_err))
                if prev_tool == &tool_name && prev_args == arguments && prev_err == &error
        );
        if looped {
            warn!(
                tool = %tool_name,
                error = %error,
                "state-spine: identical failing tool call repeated — aborting"
            );
            self.state.terminal_reason = Some(TerminalReason::LoopDetected { tool_name, error });
            return LoopStepFlow::Break;
        }
        trackers.last_failure = Some((tool_name, arguments.clone(), error));
        reset_no_progress_tracking(&mut trackers.last_action, &mut trackers.recent_actions);

        let action = recovery_strategy(
            self.state.consecutive_errors,
            self.config.max_consecutive_errors,
        );
        if matches!(action, RecoveryAction::Abort) {
            warn!(
                errors = self.state.consecutive_errors,
                "state-spine: too many consecutive errors — aborting"
            );
            self.state.terminal_reason = Some(TerminalReason::MaxErrorsReached {
                consecutive_errors: self.state.consecutive_errors,
            });
            return LoopStepFlow::Break;
        }
        LoopStepFlow::Continue
    }

    async fn handle_done_outcome<M>(&mut self, goal: &str, mcp: &M, summary: String) -> LoopStepFlow
    where
        M: Mcp + ?Sized,
    {
        let disagreement = self.verify_completion(goal, &summary, mcp).await;
        if let Some((screenshot_b64, vlm_reasoning)) = disagreement {
            warn!("state-spine: VLM disagreed with agent_done — halting for user review");
            self.emit_event(AgentEvent::CompletionDisagreement {
                screenshot_b64,
                vlm_reasoning: vlm_reasoning.clone(),
                agent_summary: summary.clone(),
            })
            .await;
            self.state.terminal_reason = Some(TerminalReason::CompletionDisagreement {
                agent_summary: summary,
                vlm_reasoning,
            });
            return LoopStepFlow::Break;
        }

        self.state.completed = true;
        self.state.summary = Some(summary.clone());
        self.state.terminal_reason = Some(TerminalReason::Completed {
            summary: summary.clone(),
        });
        self.emit_event(AgentEvent::GoalComplete { summary }).await;
        LoopStepFlow::Break
    }

    fn append_action_result_to_history(
        &mut self,
        loop_ctx: &mut RunLoopContext,
        trackers: &RunLoopTrackers,
        action: &AgentAction,
    ) {
        match action {
            AgentAction::ToolCall {
                tool_name,
                arguments,
                tool_call_id,
            } => {
                append_assistant_and_tool_result(
                    &mut loop_ctx.messages,
                    tool_name,
                    arguments,
                    tool_call_id,
                    trackers.previous_result.as_deref(),
                );
            }
            AgentAction::AgentReplan { reason } => {
                loop_ctx
                    .messages
                    .push(Message::assistant(format!("replan: {}", reason)));
            }
            AgentAction::AgentDone { .. } | AgentAction::InvokeSkill { .. } => {}
        }
    }

    /// Top-level observe → compose → LLM → parse → apply → dispatch →
    /// compact control loop. Task 3a.1 ships the minimum skeleton; later
    /// tasks (flagged by `TODO(task-3a.N)` markers inline) wire VLM
    /// verification, approval, loop detection,
    /// consecutive-destructive cap, workflow-graph emission, CDP
    /// auto-connect, synthetic `focus_window` skip, recovery strategy,
    /// and boundary `StepRecord` writes.
    ///
    /// Crate-private because the `Mcp` trait is `pub(crate)`; the public
    /// entry point stays [`crate::agent::run_agent_workflow`].
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn run<B, M>(
        mut self,
        llm: &B,
        mcp: &M,
        goal: String,
        workflow: clickweave_core::Workflow,
        mcp_tools: Vec<Value>,
        anchor_node_id: Option<uuid::Uuid>,
    ) -> anyhow::Result<AgentState>
    where
        B: ChatBackend + ?Sized,
        M: Mcp + ?Sized,
    {
        self.start_skill_watcher_if_enabled();
        // Drain queued episodic writes on *every* exit path,
        // including the early `?` returns from chat/parse failures.
        // Without this, a recovery write queued moments before an LLM
        // failure would race the Tauri-side cleanup and never commit
        // before the writer is dropped, defeating the run-terminal
        // promotion barrier the post-loop flush already installs.
        let inner = Self::run_inner(
            &mut self,
            llm,
            mcp,
            goal,
            workflow,
            mcp_tools,
            anchor_node_id,
        );
        let result = inner.await;
        if let Some(writer) = &self.episodic_writer {
            writer.flush().await;
        }
        // Spec 3: clear the per-run scratch state so the runner could
        // in theory be reused. Files (the on-disk skill store) outlive
        // the runner — only the in-memory accumulators are dropped here.
        self.recorded_steps.clear();
        self.push_idx_stack.clear();
        self.push_signature_stack.clear();
        self.last_pushed_subgoal_ids.clear();
        self.completed_subgoal_extraction_queue.clear();
        self.produced_node_ids_stack.clear();
        self.pending_applicable_skills.clear();
        self.pre_dispatch_snapshot = None;
        if let Some(handle) = self.skill_watcher_handle.take() {
            handle.abort();
        }
        match result {
            Ok(()) => Ok(self.state),
            Err(e) => Err(e),
        }
    }

    async fn run_inner<B, M>(
        &mut self,
        llm: &B,
        mcp: &M,
        goal: String,
        workflow: clickweave_core::Workflow,
        mcp_tools: Vec<Value>,
        anchor_node_id: Option<uuid::Uuid>,
    ) -> anyhow::Result<()>
    where
        B: ChatBackend + ?Sized,
        M: Mcp + ?Sized,
    {
        let mut loop_ctx = self.initialize_run_loop(&goal, workflow, &mcp_tools, anchor_node_id);
        let mut trackers = RunLoopTrackers::default();

        for _step_index in 0..self.config.max_steps {
            if self.state.completed {
                break;
            }

            // 1. Observe — refresh the compact CDP page summary, drain
            // invalidations, re-infer phase, and run episodic retrieval if
            // this iteration hits a retrieval trigger.
            let (elements, retrieved) = self.observe_for_next_turn(mcp).await;

            // 2. Compose the per-turn user message with the state block +
            // the previous tool body as the observation, then compact the
            // history before the LLM call.
            let step_obs = trackers.previous_result.clone().unwrap_or_default();
            let step_scope = self.compute_tools_in_scope(&loop_ctx.advertised_tool_names);
            // Spec 3: drain `pending_applicable_skills` once per turn —
            // the block surfaces in the next user turn after the
            // `push_subgoal` that produced it, then disappears.
            let applicable = std::mem::take(&mut self.pending_applicable_skills);
            let step_msg = build_user_turn_message_from_input(UserTurnMessageInput {
                wm: &self.world_model,
                ts: &self.task_state,
                current_step: self.step_index,
                observation_text: &step_obs,
                retrieved: &retrieved,
                applicable_skills: &applicable,
                tools_in_scope_names: &step_scope,
                max_elements: self.config.state_block_max_elements,
            });
            loop_ctx.messages.push(Message::user(step_msg));
            loop_ctx.messages = compact(loop_ctx.messages, &loop_ctx.budget);

            // 3. LLM call.
            let response = llm
                .chat(&loop_ctx.messages, Some(&loop_ctx.tools))
                .await
                .context("Agent LLM call failed")?;
            let choice = response
                .choices
                .into_iter()
                .next()
                .context("No choices in LLM response")?;

            // 4. Parse the LLM response into an AgentTurn carrying any
            //    `0..N` task-state mutations followed by exactly one
            //    action.
            let mut turn = parse_agent_turn(&choice.message)?;
            if guard_completion_after_unverified_side_effect(
                trackers.previous_result.as_deref(),
                &mut turn,
            ) {
                warn!("state-spine: blocked completion after unverified side-effectful action");
                self.emit_event(AgentEvent::Warning {
                    message: UNVERIFIED_SIDE_EFFECT_COMPLETION_BLOCKED_REASON.to_string(),
                })
                .await;
            }

            self.prepare_turn_for_dispatch(
                &mut turn,
                &mut trackers.last_action,
                &mut trackers.recent_actions,
            )
            .await;

            if self
                .handle_no_focus_launch_skip(&turn, &elements, &mut loop_ctx, &mut trackers, mcp)
                .await
            {
                continue;
            }

            force_background_launch_app(&mut turn.action, self.config.allow_focus_window);

            if self
                .handle_synthetic_focus_skip(&turn, &elements, &mut loop_ctx, &mut trackers, mcp)
                .await
            {
                continue;
            }

            match self
                .guard_runtime_managed_cdp(&turn, &elements, &mut loop_ctx, &mut trackers)
                .await
            {
                LoopStepFlow::Continue => continue,
                LoopStepFlow::Break => break,
                LoopStepFlow::Dispatch => {}
            }

            match self
                .guard_coordinate_primitive(&turn, &elements, &mut loop_ctx, &mut trackers, mcp)
                .await
            {
                LoopStepFlow::Continue => continue,
                LoopStepFlow::Break => break,
                LoopStepFlow::Dispatch => {}
            }

            match self
                .handle_permission_gate(&turn, &elements, &mut loop_ctx, &mut trackers)
                .await
            {
                LoopStepFlow::Continue => continue,
                LoopStepFlow::Break => break,
                LoopStepFlow::Dispatch => {}
            }

            // 5. Dispatch the action via run_turn. Mutations were
            //    already applied at step 4' above, so we forward an
            //    action-only turn — `run_turn`'s internal
            //    `apply_mutations` call becomes a no-op on the empty
            //    vec and `TaskStateChanged` is not emitted twice.
            //
            //    `previous_errors` captures the error counter from the
            //    iteration just before the new turn; a drop from >0 to
            //    0 after `run_turn` signals the
            //    `Recovering -> Executing` transition persisted as a
            //    `BoundaryKind::RecoverySucceeded` record.
            let previous_errors = self.consecutive_errors;
            let executor = McpToolExecutor { mcp };
            let action_only_turn = AgentTurn {
                mutations: Vec::new(),
                action: turn.action.clone(),
            };
            let (outcome, warnings, _run_turn_milestones) =
                self.run_turn(&action_only_turn, &executor).await;
            for w in warnings {
                tracing::warn!(warning = %w, "state-spine: mutation warning");
            }

            match self
                .handle_run_turn_result(
                    &goal,
                    mcp,
                    &mcp_tools,
                    &mut loop_ctx,
                    &mut trackers,
                    &turn,
                    &elements,
                    previous_errors,
                    outcome,
                )
                .await
            {
                LoopStepFlow::Break => break,
                LoopStepFlow::Continue | LoopStepFlow::Dispatch => {}
            }
        }

        // Post-loop: populate the terminal reason if the loop fell out of
        // max_steps without completing.
        if !self.state.completed && self.state.terminal_reason.is_none() {
            self.state.terminal_reason = Some(TerminalReason::MaxStepsReached {
                steps_executed: self.state.steps.len(),
            });
        }

        // Terminal boundary write (D8 / Task 3a.6.5). Every exit path from
        // the loop above sets `state.terminal_reason` before breaking —
        // plus the post-loop MaxStepsReached fallback right above — so a
        // single write here covers `Completed`, `MaxStepsReached`,
        // `MaxErrorsReached`, `ApprovalUnavailable`, `CompletionDisagreement`,
        // `ConsecutiveDestructiveCap`, and `LoopDetected` uniformly. A
        // run without any terminal_reason is a bug (no known code path
        // produces it), so the match_ is exhaustive on `Some`.
        if self.state.terminal_reason.is_some() {
            self.write_terminal_record().await;
        }

        // Drain happens in the outer `run` wrapper so it covers both
        // `Ok` and early-`?` `Err` exits from this function. See the
        // post-result `writer.flush().await` in `Self::run`.
        Ok(())
    }
}

/// Translate the openai-shaped `Vec<Value>` tool list (produced by
/// `Mcp::tools_as_openai`) into the `clickweave_mcp::Tool` shape the
/// prompt-spine builder needs. Keeps the openai format as the source of
/// truth for dispatch while letting the prompt builder operate on a typed
/// view.
fn openai_tools_to_mcp_tool_list(tools: &[Value]) -> Vec<clickweave_mcp::Tool> {
    tools
        .iter()
        .filter_map(|t| {
            let fun = t.get("function")?;
            let name = fun.get("name").and_then(Value::as_str)?.to_string();
            let description = fun
                .get("description")
                .and_then(Value::as_str)
                .map(String::from);
            let input_schema = fun
                .get("parameters")
                .cloned()
                .unwrap_or_else(|| serde_json::json!({}));
            let annotations = fun.get("annotations").cloned();
            Some(clickweave_mcp::Tool {
                name,
                description,
                input_schema,
                annotations,
            })
        })
        .collect()
}
