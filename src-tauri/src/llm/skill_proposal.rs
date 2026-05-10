use std::fmt;
use std::path::{Path, PathBuf};

use clickweave_engine::agent::skills::{
    ActionSketchStep, CaptureClause, ProvenanceEntry, Skill, SkillRefinementProposal, slugify,
};
use clickweave_llm::{ChatBackend, ChatOptions, Content, LlmClient, Message};
use serde::Serialize;
use serde_json::Value;

const SKILL_REFINEMENT_PROMPT: &str = include_str!("prompts/skill_refinement.txt");

#[derive(Debug)]
pub enum ProposalError {
    LlmError(anyhow::Error),
    EmptyResponse,
    Json(serde_json::Error),
    Io(std::io::Error),
}

impl fmt::Display for ProposalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LlmError(err) => write!(f, "LLM error: {err}"),
            Self::EmptyResponse => write!(f, "LLM returned no proposal content"),
            Self::Json(err) => write!(f, "invalid proposal JSON: {err}"),
            Self::Io(err) => write!(f, "proposal file I/O error: {err}"),
        }
    }
}

impl std::error::Error for ProposalError {}

impl From<serde_json::Error> for ProposalError {
    fn from(value: serde_json::Error) -> Self {
        Self::Json(value)
    }
}

impl From<std::io::Error> for ProposalError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

pub async fn propose_skill_refinement(
    skill: &Skill,
    contributing_traces: &[ProvenanceEntry],
    llm_client: &LlmClient,
) -> Result<SkillRefinementProposal, ProposalError> {
    propose_skill_refinement_with_backend(skill, contributing_traces, llm_client).await
}

pub async fn propose_skill_refinement_with_backend<B: ChatBackend + ?Sized>(
    skill: &Skill,
    contributing_traces: &[ProvenanceEntry],
    llm_client: &B,
) -> Result<SkillRefinementProposal, ProposalError> {
    let prompt_payload = PromptPayload::from_skill(skill, contributing_traces);
    let prompt_json = serde_json::to_string_pretty(&prompt_payload)?;
    let messages = vec![
        Message::system(SKILL_REFINEMENT_PROMPT),
        Message::user(prompt_json),
    ];
    let options = ChatOptions {
        temperature: Some(0.0),
        max_tokens: Some(2048),
    };

    let response = llm_client
        .chat_with_options(&messages, None, &options)
        .await
        .map_err(ProposalError::LlmError)?;
    let text = response
        .choices
        .first()
        .and_then(|choice| choice.message.content.as_ref())
        .and_then(Content::as_text)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or(ProposalError::EmptyResponse)?;

    parse_proposal_json(text)
}

pub fn proposal_path(skills_dir: &Path, skill: &Skill) -> PathBuf {
    skills_dir.join(format!(
        "{}-v{}.proposal.json",
        slugify(&skill.id),
        skill.version
    ))
}

pub fn write_skill_proposal(
    skills_dir: &Path,
    skill: &Skill,
    proposal: &SkillRefinementProposal,
) -> Result<PathBuf, ProposalError> {
    if !skills_dir.exists() {
        std::fs::create_dir_all(skills_dir)?;
    }
    let path = proposal_path(skills_dir, skill);
    let bytes = serde_json::to_vec_pretty(proposal)?;
    std::fs::write(&path, bytes)?;
    Ok(path)
}

fn parse_proposal_json(text: &str) -> Result<SkillRefinementProposal, ProposalError> {
    serde_json::from_str::<SkillRefinementProposal>(strip_json_fence(text)).map_err(Into::into)
}

fn strip_json_fence(text: &str) -> &str {
    let trimmed = text.trim();
    let Some(rest) = trimmed.strip_prefix("```") else {
        return trimmed;
    };
    let rest = rest.strip_prefix("json").unwrap_or(rest).trim_start();
    rest.strip_suffix("```").unwrap_or(rest).trim()
}

#[derive(Serialize)]
struct PromptPayload<'a> {
    skill: SkillPrompt<'a>,
    contributing_traces: &'a [ProvenanceEntry],
    parameter_candidates: Vec<LiteralCandidate>,
    binding_candidates: Vec<BindingCandidate>,
}

impl<'a> PromptPayload<'a> {
    fn from_skill(skill: &'a Skill, contributing_traces: &'a [ProvenanceEntry]) -> Self {
        let mut parameter_candidates = Vec::new();
        let mut binding_candidates = Vec::new();
        collect_candidates(
            &skill.action_sketch,
            String::new(),
            &mut parameter_candidates,
            &mut binding_candidates,
        );
        Self {
            skill: SkillPrompt {
                id: &skill.id,
                version: skill.version,
                name: &skill.name,
                description: &skill.description,
                subgoal_text: &skill.subgoal_text,
                parameter_schema: &skill.parameter_schema,
                action_sketch: &skill.action_sketch,
                outputs: &skill.outputs,
                outcome_predicate: &skill.outcome_predicate,
            },
            contributing_traces,
            parameter_candidates,
            binding_candidates,
        }
    }
}

#[derive(Serialize)]
struct SkillPrompt<'a> {
    id: &'a str,
    version: u32,
    name: &'a str,
    description: &'a str,
    subgoal_text: &'a str,
    parameter_schema: &'a [clickweave_engine::agent::skills::ParameterSlot],
    action_sketch: &'a [ActionSketchStep],
    outputs: &'a [clickweave_engine::agent::skills::OutputDeclaration],
    outcome_predicate: &'a clickweave_engine::agent::skills::OutcomePredicate,
}

#[derive(Serialize)]
struct LiteralCandidate {
    step_path: String,
    location: String,
    value: Value,
}

#[derive(Serialize)]
struct BindingCandidate {
    step_path: String,
    capture_name: String,
    capture: CaptureClause,
}

fn collect_candidates(
    steps: &[ActionSketchStep],
    path_prefix: String,
    literals: &mut Vec<LiteralCandidate>,
    bindings: &mut Vec<BindingCandidate>,
) {
    for (idx, step) in steps.iter().enumerate() {
        let step_path = if path_prefix.is_empty() {
            idx.to_string()
        } else {
            format!("{path_prefix}.{idx}")
        };
        match step {
            ActionSketchStep::ToolCall {
                args,
                captures_pre,
                captures,
                ..
            } => {
                collect_literals(args, &step_path, "args", literals);
                for capture in captures_pre.iter().chain(captures.iter()) {
                    bindings.push(BindingCandidate {
                        step_path: step_path.clone(),
                        capture_name: capture.name.clone(),
                        capture: capture.clone(),
                    });
                }
            }
            ActionSketchStep::Loop { body, .. } => {
                collect_candidates(body, step_path, literals, bindings);
            }
        }
    }
}

fn collect_literals(
    value: &Value,
    step_path: &str,
    location: &str,
    literals: &mut Vec<LiteralCandidate>,
) {
    match value {
        Value::String(s) if !is_binding_ref(s) => literals.push(LiteralCandidate {
            step_path: step_path.to_string(),
            location: location.to_string(),
            value: value.clone(),
        }),
        Value::Number(_) | Value::Bool(_) => literals.push(LiteralCandidate {
            step_path: step_path.to_string(),
            location: location.to_string(),
            value: value.clone(),
        }),
        Value::Array(items) => {
            for (idx, item) in items.iter().enumerate() {
                collect_literals(item, step_path, &format!("{location}[{idx}]"), literals);
            }
        }
        Value::Object(map) => {
            for (key, item) in map {
                collect_literals(item, step_path, &format!("{location}.{key}"), literals);
            }
        }
        _ => {}
    }
}

fn is_binding_ref(s: &str) -> bool {
    s.contains("{{params.") || s.contains("{{captured.")
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Result;
    use clickweave_engine::agent::skills::{
        ApplicabilityHints, ApplicabilitySignature, OutcomePredicate, SkillScope, SkillState,
        SkillStats, SubgoalSignature,
    };
    use clickweave_llm::{ChatResponse, Choice, ModelInfo};
    use std::future::Future;
    use std::sync::Mutex;

    struct StubBackend {
        response: Result<String>,
        calls: Mutex<usize>,
    }

    impl StubBackend {
        fn ok(response: &str) -> Self {
            Self {
                response: Ok(response.to_string()),
                calls: Mutex::new(0),
            }
        }

        fn err() -> Self {
            Self {
                response: Err(anyhow::anyhow!("boom")),
                calls: Mutex::new(0),
            }
        }
    }

    impl ChatBackend for StubBackend {
        fn chat_with_options(
            &self,
            _messages: &[Message],
            _tools: Option<&[Value]>,
            _options: &ChatOptions,
        ) -> impl Future<Output = Result<ChatResponse>> + Send {
            *self.calls.lock().unwrap() += 1;
            async move {
                match &self.response {
                    Ok(text) => Ok(ChatResponse {
                        id: "stub".into(),
                        choices: vec![Choice {
                            index: 0,
                            message: Message::assistant(text),
                            finish_reason: Some("stop".into()),
                        }],
                        usage: None,
                    }),
                    Err(_) => Err(anyhow::anyhow!("boom")),
                }
            }
        }

        fn model_name(&self) -> &str {
            "stub"
        }

        fn fetch_model_info(&self) -> impl Future<Output = Result<Option<ModelInfo>>> + Send {
            async { Ok(None) }
        }
    }

    #[tokio::test]
    async fn mocked_llm_returns_parsed_proposal() {
        let backend = StubBackend::ok(
            r#"{
              "parameter_schema": [{
                "name": "contact_name",
                "type_tag": "string",
                "description": "Contact to open",
                "default": null,
                "enum_values": null
              }],
              "binding_corrections": [],
              "description": "Open a contact chat.",
              "name_suggestion": "Open Contact Chat"
            }"#,
        );
        let proposal = propose_skill_refinement_with_backend(&sample_skill(), &[], &backend)
            .await
            .expect("proposal");

        assert_eq!(proposal.parameter_schema[0].name, "contact_name");
        assert_eq!(proposal.description, "Open a contact chat.");
    }

    #[tokio::test]
    async fn llm_error_is_reported_as_llm_error() {
        let backend = StubBackend::err();
        let err = propose_skill_refinement_with_backend(&sample_skill(), &[], &backend)
            .await
            .expect_err("expected LLM error");

        assert!(matches!(err, ProposalError::LlmError(_)));
    }

    fn sample_skill() -> Skill {
        let now = chrono::Utc::now();
        Skill {
            id: "open-contact-chat".into(),
            version: 1,
            state: SkillState::Draft,
            scope: SkillScope::ProjectLocal,
            name: "Open contact chat".into(),
            description: "Open a fixed contact chat".into(),
            tags: vec![],
            subgoal_text: "Open Vesna chat".into(),
            subgoal_signature: SubgoalSignature("sig".into()),
            applicability: ApplicabilityHints {
                apps: vec![],
                hosts: vec![],
                signature: ApplicabilitySignature("app".into()),
            },
            parameter_schema: vec![],
            action_sketch: vec![ActionSketchStep::ToolCall {
                step_id: "s_test_click".into(),
                tool: "click".into(),
                args: serde_json::json!({ "text": "Vesna" }),
                captures_pre: vec![],
                captures: vec![],
                expected_world_model_delta: Default::default(),
                requires_approval: None,
            }],
            outputs: vec![],
            outcome_predicate: OutcomePredicate::SubgoalCompleted {
                post_state_world_model_signature: None,
            },
            provenance: vec![],
            stats: SkillStats {
                occurrence_count: 3,
                success_rate: 1.0,
                last_seen_at: Some(now),
                last_invoked_at: None,
            },
            edited_by_user: false,
            created_at: now,
            updated_at: now,
            produced_node_ids: vec![],
            body: String::new(),
            schema_version: clickweave_engine::agent::skills::SKILL_SCHEMA_VERSION,
            variables: vec![],
            sections: vec![],
            replay: None,
        }
    }
}
