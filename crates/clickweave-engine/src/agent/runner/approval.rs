use super::*;

/// Result of requesting user approval for a tool action. Shared by both
/// policy evaluation and the live dispatch path.
pub(crate) enum ApprovalResult {
    Approved,
    Rejected,
    Unavailable,
}

/// State of the consecutive-destructive-tool cap after a tool call.
/// Mirrors the legacy `CapStatus` — private to `runner.rs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CapStatus {
    /// Streak is still below the cap — run continues normally.
    Armed,
    /// Cap reached — the caller must emit the cap-hit event and halt.
    CapReached,
}

impl StateRunner {
    pub(super) fn skill_frame_to_single_step_action(
        frame: &crate::agent::skills::SkillFrame,
    ) -> AgentAction {
        match frame.skill.action_sketch.as_slice() {
            [crate::agent::skills::ActionSketchStep::ToolCall { tool, args, .. }] => {
                match crate::agent::skills::substitution::substitute_value(
                    args,
                    &frame.params,
                    &frame.captured,
                ) {
                    Ok(arguments) => AgentAction::ToolCall {
                        tool_name: tool.clone(),
                        arguments,
                        tool_call_id: format!(
                            "skill-{}-v{}-step-{}",
                            frame.skill.id, frame.skill.version, frame.next_step
                        ),
                    },
                    Err(err) => AgentAction::AgentReplan {
                        reason: format!("skill replay substitution failed: {err}"),
                    },
                }
            }
            [] => AgentAction::AgentReplan {
                reason: format!(
                    "skill {}@v{} has no replay steps",
                    frame.skill.id, frame.skill.version
                ),
            },
            [_] => AgentAction::AgentReplan {
                reason: format!(
                    "skill {}@v{} contains a non-tool replay step; full replay is not available yet",
                    frame.skill.id, frame.skill.version
                ),
            },
            steps => AgentAction::AgentReplan {
                reason: format!(
                    "skill {}@v{} has {} replay steps; full multi-step replay is not available yet",
                    frame.skill.id,
                    frame.skill.version,
                    steps.len()
                ),
            },
        }
    }

    /// Look up the named skill, validate parameters against its
    /// schema, and emit `AgentEvent::SkillInvoked`. Returns the live
    /// [`SkillFrame`] on success or a human-readable replan reason on
    /// failure (unknown skill, draft skill, invalid parameters).
    ///
    /// Phase 4 lands the lookup-and-validate half of `dispatch_skill`.
    /// The per-step expansion through the live dispatch helper —
    /// including sub-skill recursion, the `Loop` arm, and the
    /// LLM-fallback path on divergence — is staged for the follow-up
    /// pass. See the Phase 4 deferred-items list in the handoff for
    /// the resume seam. Until that lands, the outer-loop
    /// `AgentAction::InvokeSkill` arm degrades to a replan whose reason
    /// names the skill that was about to run, so a live invocation
    /// produces a clear bail-out rather than a silent no-op.
    pub(crate) async fn dispatch_skill(
        &mut self,
        skill_id: &str,
        version: u32,
        parameters: serde_json::Value,
    ) -> Result<crate::agent::skills::SkillFrame, String> {
        use crate::agent::skills::replay::{SkillFrame, validate_parameters};
        use crate::agent::skills::types::SkillState;

        let skill = match self.skill_index.read().get(skill_id, version) {
            Some(s) if !matches!(s.state, SkillState::Draft) => s,
            Some(_) => {
                return Err(format!(
                    "skill {skill_id}@v{version} is in draft state and cannot be invoked"
                ));
            }
            None => {
                return Err(format!("unknown skill: {skill_id}@v{version}"));
            }
        };

        let validated_params = match validate_parameters(&parameters, &skill.parameter_schema) {
            Ok(p) => p,
            Err(e) => return Err(format!("invalid skill parameters: {e}")),
        };

        let parameter_count = validated_params
            .as_object()
            .map(|m| m.len() as u32)
            .unwrap_or(0);
        self.emit_event(AgentEvent::SkillInvoked {
            run_id: self.run_id,
            skill_id: skill_id.to_string(),
            version,
            parameter_count,
        })
        .await;

        // Stamp `last_invoked_at` so the index reflects the attempt
        // even when the per-step expansion hasn't landed yet.
        self.skill_index
            .write()
            .mark_invoked(skill_id, version, chrono::Utc::now());

        Ok(SkillFrame::new(skill, validated_params))
    }

    /// Best-effort send of an [`AgentEvent`] through the configured
    /// channel. No-op when the channel is unset or closed — event
    /// emission must never fail the run.
    pub(crate) async fn emit_event(&self, event: AgentEvent) {
        let Some(tx) = &self.event_tx else { return };
        if tx.is_closed() {
            return;
        }
        if let Err(e) = tx.send(RunnerOutput::Event(event)).await {
            warn!("state-spine: failed to emit agent event (channel closed): {e}");
        }
    }

    /// Update the consecutive-destructive-call tracker after a successful
    /// tool call, and report whether the cap has now been hit. Port of
    /// the legacy `AgentRunner::maybe_halt_on_destructive_cap`.
    ///
    /// `destructive_hint == Some(true)` increments the streak; anything else
    /// resets it. A cap value of `0` disables the feature entirely, so the
    /// method always returns `CapStatus::Armed` in that case.
    pub(super) fn maybe_halt_on_destructive_cap(
        &mut self,
        tool_name: &str,
        annotations_by_tool: &HashMap<String, ToolAnnotations>,
    ) -> CapStatus {
        if self.config.consecutive_destructive_cap == 0 {
            return CapStatus::Armed;
        }
        let destructive = annotations_by_tool
            .get(tool_name)
            .and_then(|a| a.destructive_hint)
            .unwrap_or(false);
        if destructive {
            self.state
                .recent_destructive_tools
                .push(tool_name.to_string());
        } else {
            self.state.recent_destructive_tools.clear();
        }
        if self.state.recent_destructive_tools.len() >= self.config.consecutive_destructive_cap {
            CapStatus::CapReached
        } else {
            CapStatus::Armed
        }
    }

    /// Halt the run because the consecutive-destructive cap was reached.
    /// Emits the cap-hit event and sets the terminal reason. Called once
    /// when `maybe_halt_on_destructive_cap` reports `CapStatus::CapReached`.
    /// Clears `recent_destructive_tools` afterwards so state serialization
    /// reflects the drained streak. Port of the legacy
    /// `AgentRunner::emit_destructive_cap_hit`.
    pub(super) async fn emit_destructive_cap_hit(&mut self) {
        let recent = std::mem::take(&mut self.state.recent_destructive_tools);
        let cap = self.config.consecutive_destructive_cap;
        warn!(
            cap,
            tools = ?recent,
            "state-spine: consecutive destructive cap reached — halting run"
        );
        self.emit_event(AgentEvent::ConsecutiveDestructiveCapHit {
            recent_tool_names: recent.clone(),
            cap,
        })
        .await;
        self.state.terminal_reason = Some(TerminalReason::ConsecutiveDestructiveCap {
            recent_tool_names: recent,
            cap,
        });
    }

    /// Evaluate the permission policy for a tool call.
    pub(super) fn policy_for(
        &self,
        tool_name: &str,
        arguments: &Value,
        annotations_by_tool: &HashMap<String, ToolAnnotations>,
    ) -> PermissionAction {
        let ann = annotations_by_tool
            .get(tool_name)
            .copied()
            .unwrap_or_default();
        evaluate_permission(&self.permissions, tool_name, arguments, &ann)
    }

    /// Prompt the operator for approval of a tool action. Port of the
    /// legacy `AgentRunner::request_approval`. Returns `None` when no
    /// approval gate is configured (auto-approve).
    ///
    /// `description_suffix` is appended to the human-facing description for
    /// callers that need extra context.
    pub(super) async fn request_approval(
        &self,
        tool_name: &str,
        arguments: &Value,
        step_index: usize,
        description_suffix: &str,
    ) -> Option<ApprovalResult> {
        let gate = self.approval_gate.as_ref()?;
        let description = format!(
            "{} with {}{}",
            tool_name,
            serde_json::to_string(arguments).unwrap_or_default(),
            description_suffix,
        );
        let request = ApprovalRequest {
            step_index,
            tool_name: tool_name.to_string(),
            arguments: arguments.clone(),
            description,
        };
        let (resp_tx, resp_rx) = oneshot::channel();
        if gate.request_tx.send((request, resp_tx)).await.is_ok() {
            match resp_rx.await {
                Ok(true) => {
                    debug!(tool = %tool_name, "state-spine: user approved action");
                    Some(ApprovalResult::Approved)
                }
                Ok(false) => {
                    tracing::info!(tool = %tool_name, "state-spine: user rejected action");
                    Some(ApprovalResult::Rejected)
                }
                Err(_) => {
                    warn!(tool = %tool_name, "state-spine: approval channel closed");
                    Some(ApprovalResult::Unavailable)
                }
            }
        } else {
            warn!(tool = %tool_name, "state-spine: approval channel send failed");
            Some(ApprovalResult::Unavailable)
        }
    }

    /// Verify an agent-reported completion against a fresh screenshot via
    /// the VLM. Port of the legacy `AgentRunner::verify_completion`.
    ///
    /// Returns the prepared base64 screenshot + VLM reply **only when the
    /// VLM disagreed** (verdict = NO). The caller uses that payload to
    /// synthesise a `CompletionDisagreement` event and terminal reason.
    /// When the VLM agrees, or any step of the verification path fails (no
    /// vision backend, screenshot failure, VLM call failure, empty reply),
    /// returns `None` and the caller falls through to the normal
    /// `Completed` path — verification errors must not tank the run.
    ///
    /// On both YES and NO verdicts, a PNG screenshot + JSON metadata are
    /// written to `self.verification_artifacts_dir` when set. Persistence
    /// failures are logged at `warn` and do not affect the return value.
    pub(super) async fn verify_completion<M: Mcp + ?Sized>(
        &mut self,
        goal: &str,
        summary: &str,
        mcp: &M,
    ) -> Option<(String, String)> {
        use crate::agent::completion_check::{
            VlmVerdict, build_completion_prompt, parse_yes_no, persist_verification_artifacts,
            pick_completion_screenshot_scope,
        };
        use crate::executor::screenshot::capture_screenshot_for_vlm;

        let vision = self.vision.as_ref()?.clone();

        // Target the screenshot scope at the connected CDP app when we
        // have one — Task 3a.6 wires `cdp_state` up via
        // `maybe_cdp_connect`, so `connected_app` now flows through to
        // the scope picker (matching legacy behaviour).
        let scope = pick_completion_screenshot_scope(self.cdp_state.connected_app.as_ref());
        let Some((prepared_b64, mime)) = capture_screenshot_for_vlm(mcp, scope.clone()).await
        else {
            warn!(
                scope = ?scope,
                "state-spine: completion verification screenshot capture failed — skipping VLM check",
            );
            return None;
        };

        let messages = vec![Message::user_with_images(
            build_completion_prompt(goal, summary),
            vec![(prepared_b64.clone(), mime)],
        )];
        let raw_reply = match vision.chat_boxed(&messages, None).await {
            Ok(resp) => resp
                .choices
                .first()
                .and_then(|c| c.message.content_text())
                .map(str::to_owned),
            Err(e) => {
                warn!(error = %e, "state-spine: VLM call failed — skipping completion check");
                return None;
            }
        };
        let reply = match raw_reply {
            Some(r) if !r.trim().is_empty() => r,
            _ => {
                warn!("state-spine: VLM returned empty reply — skipping completion check");
                return None;
            }
        };

        let verdict = parse_yes_no(&reply);

        // Persist artifacts for both verdicts so every verification call
        // leaves forensic evidence. Failures are non-fatal.
        if let Some(dir) = &self.verification_artifacts_dir {
            let ordinal = self.verification_count;
            if let Err(e) = persist_verification_artifacts(
                dir,
                ordinal,
                verdict,
                &reply,
                goal,
                summary,
                &prepared_b64,
            ) {
                warn!(
                    ordinal,
                    error = %e,
                    "state-spine: failed to persist completion-verification artifacts (non-fatal)",
                );
            }
        }
        self.verification_count += 1;

        match verdict {
            VlmVerdict::Yes => {
                tracing::info!(reply = %reply, "state-spine: VLM confirmed completion");
                None
            }
            VlmVerdict::No => {
                tracing::info!(reply = %reply, "state-spine: VLM rejected completion");
                Some((prepared_b64, reply))
            }
        }
    }
}
