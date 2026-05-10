use super::*;

impl StateRunner {
    /// Apply one `AgentTurn` in the state-spine control flow:
    ///
    /// 1. Apply mutations in order (errors become warnings, not fatal).
    /// 2. Observe (absorb any queued invalidation events + re-infer phase).
    /// 3. Dispatch the action:
    ///     - `ToolCall`: call the executor, update continuity on success,
    ///       queue `ToolFailed` and bump `consecutive_errors` on error.
    ///     - `AgentDone` / `AgentReplan`: return the terminal outcome.
    /// 4. Advance `step_index`.
    ///
    /// Integration tests drive this with deterministic `AgentTurn`s; Phase 3
    /// is wrapped by the LLM loop + compaction in [`Self::run_inner`].
    ///
    /// Return tuple: `(outcome, warnings, milestones_appended)`.
    /// `milestones_appended` counts `CompleteSubgoal` mutations that
    /// successfully popped a subgoal off the stack during this turn.
    /// In the live runner the outer loop applies mutations *before*
    /// calling `run_turn` (so `run_turn` receives an action-only turn
    /// and the count returned here is `0`); the count is meaningful
    /// for integration tests that drive `run_turn` directly with
    /// non-empty mutation batches.
    pub async fn run_turn<E: ToolExecutor + ?Sized>(
        &mut self,
        turn: &AgentTurn,
        executor: &E,
    ) -> (TurnOutcome, Vec<String>, usize) {
        // 1. Apply mutations first — phase inference reads the stack/watch state.
        //    Count successful `CompleteSubgoal` mutations by diffing the
        //    milestones vec length (each `CompleteSubgoal` that passes
        //    validation appends exactly one `Milestone`; see
        //    `TaskState::apply`). Milestones don't shrink during normal
        //    operation, so the delta is an exact count of new milestones.
        let milestones_before = self.task_state.milestones.len();
        let warnings = self.apply_mutations(&turn.mutations);
        let milestones_appended = self
            .task_state
            .milestones
            .len()
            .saturating_sub(milestones_before);

        // 1a. Emit `TaskStateChanged` once per turn when `apply_mutations`
        //     had anything to apply (D17). The event reflects the full
        //     post-mutation state so subscribers never have to reassemble
        //     it from the warnings vec.
        if !turn.mutations.is_empty() {
            self.emit_event(AgentEvent::TaskStateChanged {
                run_id: self.run_id,
                task_state: self.task_state.clone(),
            })
            .await;
        }

        // 2. Observe: snapshot field signatures → drain pending events +
        //    re-infer phase → compute diff → emit `WorldModelChanged` (D17).
        //    If `run()` captured signatures before its observe-phase
        //    mirror (`fetch_cdp_page_summary` → `world_model.cdp_page`)
        //    use that baseline so direct-observation writes also surface
        //    in `changed_fields`; otherwise (unit/test callers) fall back
        //    to snapshotting here.
        let pre_signatures = self
            .turn_pre_signatures
            .take()
            .unwrap_or_else(|| self.world_model.field_signatures());
        let prev_phase = self.task_state.phase;
        self.observe();
        if prev_phase != self.task_state.phase {
            self.emit_event(AgentEvent::TaskStateChanged {
                run_id: self.run_id,
                task_state: self.task_state.clone(),
            })
            .await;
        }
        let post_signatures = self.world_model.field_signatures();
        let diff = diff_world_model_signatures(&pre_signatures, &post_signatures);
        self.emit_event(AgentEvent::WorldModelChanged {
            run_id: self.run_id,
            diff,
        })
        .await;

        // 3. Dispatch action.
        let outcome = match &turn.action {
            AgentAction::ToolCall {
                tool_name,
                arguments,
                ..
            } => match executor.call_tool(tool_name, arguments).await {
                Ok(body) => {
                    self.update_continuity_after_tool_success(tool_name, &body);
                    self.queue_invalidations_for_tool_success(tool_name, arguments);
                    self.consecutive_errors = 0;
                    TurnOutcome::ToolSuccess {
                        tool_name: tool_name.clone(),
                        tool_body: body,
                    }
                }
                Err(error) => {
                    self.consecutive_errors += 1;
                    let stale_cdp_uid = is_stale_cdp_uid_error(tool_name, &error);
                    if stale_cdp_uid {
                        self.world_model.elements = None;
                    }
                    let error = if stale_cdp_uid {
                        build_stale_cdp_uid_nudge(&error)
                    } else {
                        error
                    };
                    self.queue_invalidation(InvalidationEvent::ToolFailed {
                        tool: tool_name.clone(),
                    });
                    TurnOutcome::ToolError {
                        tool_name: tool_name.clone(),
                        error,
                    }
                }
            },
            AgentAction::AgentDone { summary } => TurnOutcome::Done {
                summary: summary.clone(),
            },
            AgentAction::AgentReplan { reason } => {
                self.last_replan_step = Some(self.step_index);
                TurnOutcome::Replan {
                    reason: reason.clone(),
                }
            }
            AgentAction::InvokeSkill {
                skill_id,
                version,
                parameters,
            } => {
                // Phase 4: validate the skill exists + parameter
                // shape + emit `SkillInvoked`. The per-step expansion
                // (Task 4.3 follow-up) hasn't landed yet, so this arm
                // returns a replan that names the resolved skill so
                // the next LLM turn has a clear breadcrumb. Errors at
                // lookup / validation time produce an `InvalidArgs`-
                // shaped replan instead of panicking so a malformed
                // `invoke_skill` call can't take the run down.
                match self
                    .dispatch_skill(skill_id, *version, parameters.clone())
                    .await
                {
                    Ok(frame) => TurnOutcome::Replan {
                        reason: format!(
                            "skill {}@v{} resolved with {} parameter(s); replay engine pending — falling back to LLM",
                            frame.skill.id,
                            frame.skill.version,
                            frame.params.as_object().map(|m| m.len()).unwrap_or(0),
                        ),
                    },
                    Err(reason) => TurnOutcome::Replan { reason },
                }
            }
            AgentAction::SkillPatch {
                patch,
                tool_name,
                parse_error,
            } => {
                // Patch synthesis happened inside `parse_agent_turn`. If
                // parsing failed the synthesizer logged a warning there;
                // here we surface a replan with the error text so the LLM
                // can correct its arguments on the next turn instead of
                // spinning silently.
                //
                // When synthesis succeeded the patch is applied in a later
                // phase (Phase N — full disk write + lint + sidecar). For
                // now we return a success body that names the patch so the
                // transcript shows the intent and the LLM can continue.
                match patch {
                    None => {
                        let err = parse_error
                            .as_deref()
                            .unwrap_or("unknown patch synthesis error");
                        TurnOutcome::Replan {
                            reason: format!(
                                "{tool_name}: patch synthesis failed — {err}"
                            ),
                        }
                    }
                    Some(p) => {
                        let body = format!(
                            r#"{{"ok":true,"tool":"{tool_name}","skill_id":"{skill_id}","primitive":"{primitive:?}"}}"#,
                            tool_name = tool_name,
                            skill_id = p.skill_id,
                            primitive = p.primitive,
                        );
                        TurnOutcome::ToolSuccess {
                            tool_name: tool_name.clone(),
                            tool_body: body,
                        }
                    }
                }
            }
        };

        // `step_index` is owned by the outer-loop call sites that record
        // an `AgentStep` (via `advance_recorded_step_index`). `run_turn`
        // intentionally does not advance it — early-continue paths
        // (synthetic focus skip, policy deny, approval reject) record
        // their own steps without going through
        // `run_turn`, and prior to this fix the divergent advancement
        // let `step_index == 0` re-fire D24 run-start retrieval after
        // the run had already taken actions.

        (outcome, warnings, milestones_appended)
    }

    /// Advance the recorded-step counter. Single owner of `step_index`
    /// updates. Call after every `self.state.steps.push(...)` site so
    /// `step_index` matches `state.steps.len()` and the prompt's
    /// rendered step number stays in sync with what the run has
    /// actually executed.
    pub(crate) fn advance_recorded_step_index(&mut self) {
        self.step_index += 1;
    }

    /// Emit a per-step `WorldModelChanged` event for an early-exit step
    /// path that recorded an `AgentStep` without going through
    /// `run_turn`. Live policy-deny, live approval-reject, and the synthetic
    /// `focus_window` skip all record steps but skip `run_turn`
    /// entirely; without this hook, the `turn_pre_signatures` baseline
    /// would be carried into the next iteration and the
    /// `WorldModelChanged` diff would span multiple recorded steps.
    ///
    /// Consumes the current baseline (top-of-loop snapshot) and
    /// re-seeds it with the post-step signatures so the next iteration
    /// sees a fresh baseline keyed to the just-recorded step.
    pub(crate) async fn emit_world_model_changed_for_recorded_step(&mut self) {
        let pre_signatures = self
            .turn_pre_signatures
            .take()
            .unwrap_or_else(|| self.world_model.field_signatures());
        let post_signatures = self.world_model.field_signatures();
        let diff = diff_world_model_signatures(&pre_signatures, &post_signatures);
        self.emit_event(AgentEvent::WorldModelChanged {
            run_id: self.run_id,
            diff,
        })
        .await;
        self.turn_pre_signatures = Some(post_signatures);
    }

    /// Record a permission-policy denial as the current "last failure"
    /// so any subsequent `Recovering`-entry snapshot captures a real
    /// `(failed_tool, error_kind)` pair instead of the empty defaults.
    /// `error_kind` is the stable string `"policy_denied"` so episodic
    /// retrieval can group denied-tool recoveries by failure family
    /// without parsing the human-readable message.
    pub(crate) fn record_policy_deny_failure(&mut self, tool_name: &str) {
        self.last_failed_tool_name = Some(tool_name.to_string());
        self.last_failed_error_kind = Some("policy_denied".to_string());
    }

    /// Mirror of `record_policy_deny_failure`'s clear half. Called by
    /// every recovery-success path (live ToolSuccess in `run_turn`,
    /// synthetic focus-window skip) so a prior
    /// deny / tool-error doesn't bleed into a later Recovering snapshot
    /// after the agent has demonstrably recovered.
    pub(crate) fn clear_last_failure_tracking(&mut self) {
        self.last_failed_tool_name = None;
        self.last_failed_error_kind = None;
    }

    /// Bump the success-side repeat-action tracker for one dispatched
    /// non-observation tool call. Returns the no-progress nudge string
    /// when the streak crosses [`REPEAT_ACTION_THRESHOLD`], `None`
    /// otherwise. Caller installs the nudge into `previous_result` so
    /// the next turn renders it as the observation; the warning event
    /// is emitted here.
    ///
    /// Called by the live `ToolSuccess` arm so repeated live dispatches
    /// contribute to the same streak count.
    pub(super) async fn track_repeat_action(
        &mut self,
        tool_name: &str,
        tool_arguments: &Value,
        tool_body: &str,
        annotations_by_tool: &HashMap<String, ToolAnnotations>,
        last_action: &mut Option<LastActionProgress>,
        recent_actions: &mut VecDeque<ActionProgressSignature>,
    ) -> Option<String> {
        if is_observation_tool(tool_name, annotations_by_tool) {
            return None;
        }
        let context_signature = stable_no_progress_context_signature(&self.world_model);
        if last_action
            .as_ref()
            .is_some_and(|last| last.context_signature != context_signature)
        {
            *last_action = None;
            recent_actions.clear();
        }
        let signature = ActionProgressSignature {
            tool_name: tool_name.to_string(),
            arguments: tool_arguments.clone(),
            context_signature: context_signature.clone(),
        };
        if recent_actions.len() == ACTION_CYCLE_WINDOW {
            recent_actions.pop_front();
        }
        recent_actions.push_back(signature);
        let same_as_last = matches!(
            last_action.as_ref(),
            Some(last)
                if last.tool_name == tool_name
                    && last.arguments == *tool_arguments
                    && last.context_signature == context_signature
        );
        let count = if same_as_last {
            last_action.as_ref().map(|last| last.count).unwrap_or(0) + 1
        } else {
            1
        };
        *last_action = Some(LastActionProgress {
            tool_name: tool_name.to_string(),
            arguments: tool_arguments.clone(),
            context_signature,
            count,
        });
        if count < REPEAT_ACTION_THRESHOLD {
            if let Some(cycle) = detect_repeated_action_cycle(recent_actions) {
                let cycle_summary = cycle.join(" -> ");
                warn!(
                    cycle = %cycle_summary,
                    "state-spine: repeated action cycle detected — injecting no-progress nudge"
                );
                self.emit_event(AgentEvent::Warning {
                    message: format!(
                        "{}: repeated action cycle `{}`",
                        NO_PROGRESS_WARNING_PREFIX, cycle_summary
                    ),
                })
                .await;
                return Some(build_action_cycle_nudge(&cycle_summary, tool_body));
            }
            return None;
        }
        warn!(
            tool = %tool_name,
            count,
            "state-spine: repeat-action threshold reached — injecting no-progress nudge"
        );
        self.emit_event(AgentEvent::Warning {
            message: format!(
                "{}: `{}` repeated {} turns in a row",
                NO_PROGRESS_WARNING_PREFIX, tool_name, count
            ),
        })
        .await;
        Some(build_no_progress_nudge(tool_name, count, tool_body))
    }

    pub(super) async fn track_post_text_submit_search(
        &mut self,
        tool_name: &str,
        tool_arguments: &Value,
        tool_body: &str,
        pending: &mut Option<TextSubmitSearchProgress>,
    ) -> Option<String> {
        if is_text_composition_tool(tool_name) {
            *pending = Some(TextSubmitSearchProgress {
                context_signature: stable_no_progress_context_signature(&self.world_model),
                count: 0,
            });
            return None;
        }

        if tool_name != "cdp_find_elements" {
            if !OBSERVATION_TOOLS.contains(&tool_name) {
                *pending = None;
            }
            return None;
        }

        if !is_send_submit_cdp_search(tool_arguments) {
            return None;
        }

        let Some(progress) = pending.as_mut() else {
            return None;
        };
        let context_signature = stable_no_progress_context_signature(&self.world_model);
        if progress.context_signature != context_signature {
            *pending = None;
            return None;
        }

        if cdp_find_elements_has_matches(tool_body) != Some(false) {
            progress.count = 0;
            return None;
        }

        progress.count += 1;
        if progress.count < TEXT_SUBMIT_SEARCH_THRESHOLD {
            return None;
        }

        warn!(
            count = progress.count,
            "state-spine: repeated post-text send search detected — injecting no-progress nudge"
        );
        self.emit_event(AgentEvent::Warning {
            message: format!(
                "{}: repeated send/submit search after composing text",
                NO_PROGRESS_WARNING_PREFIX
            ),
        })
        .await;
        Some(build_post_text_submit_nudge(progress.count, tool_body))
    }
}
