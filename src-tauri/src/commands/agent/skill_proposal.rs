use super::*;

pub(super) fn maybe_spawn_skill_proposal_task(
    event: &AgentEvent,
    skill_ctx: &SkillContext,
    agent_config: clickweave_llm::LlmConfig,
) {
    let AgentEvent::SkillExtracted {
        skill_id,
        version,
        state,
        scope,
        ..
    } = event
    else {
        return;
    };
    if !skill_ctx.enabled || *state != SkillState::Draft || *scope != SkillScope::ProjectLocal {
        return;
    }

    spawn_skill_proposal_task(skill_ctx, agent_config, skill_id.clone(), *version);
}

async fn wait_for_agent_event_drain(event_tx: &tokio::sync::mpsc::Sender<RunnerOutput>) {
    let (ack_tx, ack_rx) = tokio::sync::oneshot::channel();
    if event_tx
        .send(RunnerOutput::DrainBarrier { ack: ack_tx })
        .await
        .is_ok()
    {
        let _ = ack_rx.await;
    }
}

pub(super) async fn emit_after_agent_event_drain<R: tauri::Runtime>(
    event_tx: &tokio::sync::mpsc::Sender<RunnerOutput>,
    app: &tauri::AppHandle<R>,
    topic: &str,
    payload: serde_json::Value,
) {
    wait_for_agent_event_drain(event_tx).await;
    let _ = app.emit(topic, payload);
}

pub(super) fn spawn_skill_proposal_task(
    skill_ctx: &SkillContext,
    agent_config: clickweave_llm::LlmConfig,
    skill_id: String,
    version: u32,
) {
    let skills_dir = skill_ctx.project_skills_dir.clone();
    tauri::async_runtime::spawn(async move {
        let store = SkillStore::new(skills_dir.clone());
        let skill_path = store.skill_md_path(&skill_id);
        let Ok(skill) = store.read_skill(&skill_path) else {
            tracing::warn!(%skill_id, version, "skills: proposal task could not read skill file");
            return;
        };
        if skill.state != SkillState::Draft || skill.stats.occurrence_count < 3 {
            return;
        }
        let proposal_path = crate::llm::skill_proposal::proposal_path(&skills_dir, &skill);
        if proposal_path.exists() {
            return;
        }

        let mut provenance = skill.provenance.clone();
        provenance.sort_by_key(|p| p.completed_at);
        let start = provenance.len().saturating_sub(3);
        let contributing = provenance[start..].to_vec();

        let llm =
            clickweave_llm::LlmClient::new(agent_config.with_thinking(false).with_max_tokens(2048));
        match crate::llm::skill_proposal::propose_skill_refinement(&skill, &contributing, &llm)
            .await
        {
            Ok(proposal) => {
                if let Err(err) =
                    crate::llm::skill_proposal::write_skill_proposal(&skills_dir, &skill, &proposal)
                {
                    tracing::warn!(%skill_id, version, error = %err, "skills: failed to write proposal");
                }
            }
            Err(err) => {
                tracing::warn!(%skill_id, version, error = %err, "skills: proposal generation failed");
            }
        }
    });
}
