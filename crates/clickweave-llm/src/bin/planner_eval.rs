use anyhow::{Context, Result};
use clap::Parser;
use clickweave_core::{NodeRole, NodeType, Workflow};
use clickweave_llm::planner::{
    PatchResult, patch_workflow_with_backend, plan_workflow_with_backend,
};
use clickweave_llm::{LlmClient, LlmConfig};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Instant;
use tokio::sync::Semaphore;
use uuid::Uuid;

// ── CLI ─────────────────────────────────────────────────────────

#[derive(Parser)]
#[command(name = "planner-eval", about = "Evaluate planner prompt quality")]
struct Cli {
    /// Path to eval config file
    #[arg(long, default_value = "eval/eval.toml")]
    config: PathBuf,

    /// Run a single case by name (substring match)
    #[arg(long)]
    case: Option<String>,

    /// Override runs per case
    #[arg(long)]
    runs: Option<u32>,

    /// Override prompt template path
    #[arg(long)]
    prompt: Option<PathBuf>,

    /// Override model name
    #[arg(long)]
    model: Option<String>,

    /// Max concurrent LLM requests (default: 4)
    #[arg(long, default_value = "4")]
    concurrency: usize,
}

// ── Config types ────────────────────────────────────────────────

#[derive(Deserialize)]
struct EvalConfig {
    llm: LlmSection,
    prompts: PromptsSection,
    run: RunSection,
}

#[derive(Deserialize)]
struct LlmSection {
    /// Single endpoint (backwards-compatible).
    #[serde(default)]
    endpoint: Option<String>,
    /// Multiple weighted endpoints.
    #[serde(default)]
    endpoints: Vec<EndpointEntry>,
    model: String,
    #[serde(default)]
    api_key: Option<String>,
    #[serde(default)]
    temperature: Option<f32>,
}

/// An endpoint entry — accepts either a plain string or a table with url + weight.
#[derive(Deserialize, Clone)]
#[serde(untagged)]
enum EndpointEntry {
    Url(String),
    Weighted {
        url: String,
        #[serde(default = "default_weight")]
        weight: u32,
    },
}

fn default_weight() -> u32 {
    1
}

impl EndpointEntry {
    fn url(&self) -> &str {
        match self {
            Self::Url(u) => u,
            Self::Weighted { url, .. } => url,
        }
    }
    fn weight(&self) -> u32 {
        match self {
            Self::Url(_) => 1,
            Self::Weighted { weight, .. } => *weight,
        }
    }
}

impl LlmSection {
    /// Build a weighted assignment schedule. Each entry maps to a client index,
    /// repeated by weight. E.g. weights [2, 1] → [0, 0, 1].
    fn resolve(&self) -> Result<(Vec<String>, Vec<usize>)> {
        let mut entries: Vec<(String, u32)> = self
            .endpoints
            .iter()
            .map(|e| (e.url().to_string(), e.weight()))
            .collect();
        if let Some(ep) = &self.endpoint {
            if !entries.iter().any(|(u, _)| u == ep) {
                entries.insert(0, (ep.clone(), 1));
            }
        }
        if entries.is_empty() {
            anyhow::bail!("No LLM endpoints configured — set 'endpoint' or 'endpoints' in [llm]");
        }
        let urls: Vec<String> = entries.iter().map(|(u, _)| u.clone()).collect();
        let mut schedule: Vec<usize> = Vec::new();
        for (idx, (_, weight)) in entries.iter().enumerate() {
            for _ in 0..*weight {
                schedule.push(idx);
            }
        }
        Ok((urls, schedule))
    }
}

#[derive(Deserialize)]
struct PromptsSection {
    planner: String,
}

#[derive(Deserialize)]
struct RunSection {
    cases_dir: String,
    tools_file: String,
    #[serde(default = "default_runs")]
    runs_per_case: u32,
}

fn default_runs() -> u32 {
    3
}

// ── Case types ──────────────────────────────────────────────────

#[derive(Deserialize)]
struct EvalCase {
    name: String,
    turns: Vec<EvalTurn>,
}

#[derive(Deserialize, Clone)]
struct EvalTurn {
    prompt: String,
    #[serde(default)]
    expect: Option<CaseExpectations>,
}

#[derive(Deserialize, Default, Clone)]
struct CaseExpectations {
    /// Whether the plan should parse successfully. Deserialized from TOML
    /// but not checked explicitly -- a successful parse implies valid=true.
    #[serde(default)]
    #[allow(dead_code)]
    valid: Option<bool>,
    #[serde(default)]
    min_nodes: Option<usize>,
    #[serde(default)]
    max_nodes: Option<usize>,
    #[serde(default)]
    required_tools: Vec<String>,
    #[serde(default)]
    required_patterns: Vec<String>,
}

// ── Result types ────────────────────────────────────────────────

#[derive(Serialize)]
struct TurnResult {
    valid: bool,
    node_count: usize,
    tools_found: Vec<String>,
    patterns_found: Vec<String>,
    warnings: Vec<String>,
    workflow: Option<Workflow>,
    error: Option<String>,
}

impl TurnResult {
    fn error(msg: String) -> Self {
        Self {
            valid: false,
            node_count: 0,
            tools_found: vec![],
            patterns_found: vec![],
            warnings: vec![],
            workflow: None,
            error: Some(msg),
        }
    }
}

#[derive(Serialize)]
struct RunResult {
    endpoint: String,
    turn_results: Vec<TurnResult>,
}

// ── Config loading ──────────────────────────────────────────────

fn load_config(path: &Path) -> Result<EvalConfig> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read config: {}", path.display()))?;
    toml_edit::de::from_str(&content)
        .with_context(|| format!("Failed to parse config: {}", path.display()))
}

fn discover_cases(cases_dir: &Path, filter: Option<&str>) -> Result<Vec<EvalCase>> {
    let mut entries: Vec<_> = std::fs::read_dir(cases_dir)
        .with_context(|| format!("Failed to read cases dir: {}", cases_dir.display()))?
        .collect::<std::io::Result<Vec<_>>>()
        .with_context(|| format!("Failed to read entry in: {}", cases_dir.display()))?
        .into_iter()
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "toml"))
        .collect();
    entries.sort_by_key(|e| e.file_name());

    let mut cases = Vec::new();
    for entry in entries {
        let content = std::fs::read_to_string(entry.path())?;
        let case: EvalCase = toml_edit::de::from_str(&content)
            .with_context(|| format!("Failed to parse case: {}", entry.path().display()))?;
        if let Some(name_filter) = filter
            && !case
                .name
                .to_lowercase()
                .contains(&name_filter.to_lowercase())
        {
            continue;
        }
        cases.push(case);
    }
    Ok(cases)
}

// ── Scoring ─────────────────────────────────────────────────────

fn score_run(workflow: &Workflow, expect: &CaseExpectations) -> bool {
    if let Some(min) = expect.min_nodes
        && workflow.nodes.len() < min
    {
        return false;
    }
    if let Some(max) = expect.max_nodes
        && workflow.nodes.len() > max
    {
        return false;
    }

    let tools = extract_tools(workflow);
    if !expect.required_tools.iter().all(|t| tools.contains(t)) {
        return false;
    }

    let patterns = extract_patterns(workflow);
    expect
        .required_patterns
        .iter()
        .all(|p| patterns.contains(p))
}

fn extract_tools(workflow: &Workflow) -> HashSet<String> {
    workflow
        .nodes
        .iter()
        .filter_map(|n| {
            clickweave_core::tool_mapping::node_type_to_tool_invocation(&n.node_type)
                .ok()
                .map(|inv| inv.name)
        })
        .collect()
}

fn extract_patterns(workflow: &Workflow) -> HashSet<String> {
    let mut patterns = HashSet::new();
    for node in &workflow.nodes {
        match &node.node_type {
            NodeType::Loop(_) => {
                patterns.insert("loop".to_string());
            }
            NodeType::If(_) => {
                patterns.insert("conditional".to_string());
            }
            _ => {}
        }
        if node.role == NodeRole::Verification {
            patterns.insert("verification".to_string());
        }
    }
    patterns
}

fn sorted_tools(workflow: &Workflow) -> Vec<String> {
    let mut tools: Vec<_> = extract_tools(workflow).into_iter().collect();
    tools.sort();
    tools
}

fn sorted_patterns(workflow: &Workflow) -> Vec<String> {
    let mut patterns: Vec<_> = extract_patterns(workflow).into_iter().collect();
    patterns.sort();
    patterns
}

fn apply_patch(workflow: &Workflow, patch: &PatchResult) -> Workflow {
    let removed_ids: HashSet<Uuid> = patch.removed_node_ids.iter().copied().collect();
    let removed_edge_keys: HashSet<(Uuid, Uuid)> =
        patch.removed_edges.iter().map(|e| (e.from, e.to)).collect();

    let nodes: Vec<_> = workflow
        .nodes
        .iter()
        .filter(|n| !removed_ids.contains(&n.id))
        .map(|n| {
            patch
                .updated_nodes
                .iter()
                .find(|u| u.id == n.id)
                .cloned()
                .unwrap_or_else(|| n.clone())
        })
        .chain(patch.added_nodes.iter().cloned())
        .collect();

    let node_ids: HashSet<Uuid> = nodes.iter().map(|n| n.id).collect();
    let edges: Vec<_> = workflow
        .edges
        .iter()
        .filter(|e| !removed_edge_keys.contains(&(e.from, e.to)))
        .filter(|e| node_ids.contains(&e.from) && node_ids.contains(&e.to))
        .cloned()
        .chain(patch.added_edges.iter().cloned())
        .collect();

    let mut merged = Workflow {
        nodes,
        edges,
        ..workflow.clone()
    };
    merged.fixup_auto_ids();
    merged
}

// ── Helpers ─────────────────────────────────────────────────────

fn resolve_path(crate_root: &Path, relative: &str) -> PathBuf {
    let p = PathBuf::from(relative);
    if p.is_relative() {
        crate_root.join(p)
    } else {
        p
    }
}

fn short_hash(content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    format!("{:x}", hasher.finalize())[..8].to_string()
}

// ── Progress tracking ───────────────────────────────────────────

struct Progress {
    completed: AtomicU32,
    total: u32,
    start: Instant,
}

impl Progress {
    fn new(total: u32) -> Self {
        Self {
            completed: AtomicU32::new(0),
            total,
            start: Instant::now(),
        }
    }

    fn tick(&self) {
        let done = self.completed.fetch_add(1, Ordering::Relaxed) + 1;
        let elapsed = self.start.elapsed().as_secs_f64();
        let runs_per_min = if elapsed > 0.0 {
            done as f64 / elapsed * 60.0
        } else {
            0.0
        };
        eprint!(
            "\r  [{}/{}] {:.0}s elapsed, {:.1} runs/min    ",
            done, self.total, elapsed, runs_per_min,
        );
    }

    fn finish(&self) {
        let elapsed = self.start.elapsed().as_secs_f64();
        let done = self.completed.load(Ordering::Relaxed);
        let runs_per_min = if elapsed > 0.0 {
            done as f64 / elapsed * 60.0
        } else {
            0.0
        };
        eprintln!(
            "\r  [{}/{}] done in {:.0}s ({:.1} runs/min)        ",
            done, self.total, elapsed, runs_per_min,
        );
    }
}

// ── Results output ──────────────────────────────────────────────

struct EvalSummary<'a> {
    model: &'a str,
    prompt_path: &'a str,
    prompt_hash: &'a str,
    runs_per_case: u32,
    total_passed: u32,
    total_runs: u32,
}

fn write_results(
    crate_root: &Path,
    summary: &EvalSummary,
    cases: &[serde_json::Value],
) -> Result<()> {
    let results_dir = crate_root.join("eval/results");
    std::fs::create_dir_all(&results_dir)?;

    let now = chrono::Utc::now();
    let pass_rate = if summary.total_runs > 0 {
        (summary.total_passed as f64 * 100.0 / summary.total_runs as f64).round() as u32
    } else {
        0
    };

    let results = serde_json::json!({
        "timestamp": now.format("%Y-%m-%dT%H:%M:%SZ").to_string(),
        "model": summary.model,
        "prompt_file": summary.prompt_path,
        "prompt_hash": summary.prompt_hash,
        "runs_per_case": summary.runs_per_case,
        "cases": cases,
        "overall": {
            "total_runs": summary.total_runs,
            "passed": summary.total_passed,
            "pass_rate": format!("{pass_rate}%"),
        }
    });

    let sanitized_model = summary.model.replace('/', "_");
    let filename = format!("{}_{sanitized_model}.json", now.format("%Y-%m-%d_%H-%M-%S"));
    let path = results_dir.join(&filename);
    std::fs::write(&path, serde_json::to_string_pretty(&results)?)?;
    println!("\n  Results written to: {}", path.display());

    Ok(())
}

// ── Main ────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let config = load_config(&resolve_path(&crate_root, &cli.config.to_string_lossy()))?;

    // Apply CLI overrides
    let model = cli.model.as_deref().unwrap_or(&config.llm.model);
    let runs_per_case = cli.runs.unwrap_or(config.run.runs_per_case);
    if runs_per_case == 0 {
        anyhow::bail!("--runs must be at least 1");
    }
    let prompt_rel = cli
        .prompt
        .as_deref()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| config.prompts.planner.clone());
    let prompt_template = std::fs::read_to_string(resolve_path(&crate_root, &prompt_rel))
        .context("Failed to read prompt template")?;
    let prompt_hash = short_hash(&prompt_template);

    // Load tools fixture
    let tools_path = resolve_path(&crate_root, &config.run.tools_file);
    let tools_json: Vec<serde_json::Value> = serde_json::from_str(
        &std::fs::read_to_string(&tools_path).context("Failed to read tools file")?,
    )
    .context("Failed to parse tools file")?;

    // Load cases
    let cases_dir = resolve_path(&crate_root, &config.run.cases_dir);
    let cases = discover_cases(&cases_dir, cli.case.as_deref())?;

    if cases.is_empty() {
        anyhow::bail!("No cases found");
    }

    let total_runs = cases.len() as u32 * runs_per_case;

    // Create LLM clients — one per endpoint, weighted round-robin assigned to tasks.
    let (urls, schedule) = config.llm.resolve()?;
    let clients: Vec<Arc<LlmClient>> = urls
        .iter()
        .map(|ep| {
            Arc::new(LlmClient::new(LlmConfig {
                base_url: ep
                    .trim_end_matches('/')
                    .trim_end_matches("/chat/completions")
                    .to_string(),
                api_key: config.llm.api_key.clone(),
                model: model.to_string(),
                temperature: config.llm.temperature,
                max_tokens: Some(4096),
                ..LlmConfig::default()
            }))
        })
        .collect();

    let ep_label = if urls.len() == 1 {
        urls[0].clone()
    } else {
        let weights: Vec<String> = urls
            .iter()
            .map(|u| {
                let w = schedule.iter().filter(|&&i| urls[i] == *u).count();
                format!("{}(w={})", u, w)
            })
            .collect();
        format!("{} endpoints: {}", urls.len(), weights.join(", "))
    };
    println!(
        "planner-eval — model: {model}, {ep_label}, prompt: {prompt_rel} ({prompt_hash}), {total_runs} runs ({}x{})\n",
        cases.len(),
        runs_per_case,
    );

    // Per-endpoint semaphores so a slow/dead endpoint can't starve others.
    let per_endpoint_sems: Vec<Arc<Semaphore>> = urls
        .iter()
        .enumerate()
        .map(|(idx, _)| {
            let weight = schedule.iter().filter(|&&i| i == idx).count();
            // Scale concurrency by weight proportion, minimum 1.
            let slots = (cli.concurrency * weight / schedule.len()).max(1);
            Arc::new(Semaphore::new(slots))
        })
        .collect();

    let tools_json = Arc::new(tools_json);
    let prompt_template = Arc::new(prompt_template);
    let progress = Arc::new(Progress::new(total_runs));

    let mut handles = Vec::new();
    let schedule_len = schedule.len();
    for (case_idx, case) in cases.iter().enumerate() {
        for run_idx in 0..runs_per_case {
            let task_idx = case_idx * runs_per_case as usize + run_idx as usize;
            let client_idx = schedule[task_idx % schedule_len];
            let llm = Arc::clone(&clients[client_idx]);
            let endpoint = urls[client_idx].clone();
            let tools = Arc::clone(&tools_json);
            let template = Arc::clone(&prompt_template);
            let sem = Arc::clone(&per_endpoint_sems[client_idx]);
            let prog = Arc::clone(&progress);
            let turns = case.turns.clone();

            handles.push((
                case_idx,
                run_idx,
                tokio::spawn(async move {
                    let _permit = sem.acquire().await.unwrap();
                    let mut current_workflow: Option<Workflow> = None;
                    let mut turn_results: Vec<TurnResult> = Vec::new();

                    for (turn_idx, turn) in turns.iter().enumerate() {
                        let turn_result = if turn_idx == 0 {
                            // Turn 1: plan from scratch
                            match plan_workflow_with_backend(
                                llm.as_ref(),
                                &turn.prompt,
                                &tools,
                                false,
                                false,
                                Some(&template),
                                None,
                            )
                            .await
                            {
                                Ok(plan_result) => {
                                    current_workflow = Some(plan_result.workflow.clone());
                                    TurnResult {
                                        valid: true,
                                        node_count: plan_result.workflow.nodes.len(),
                                        tools_found: sorted_tools(&plan_result.workflow),
                                        patterns_found: sorted_patterns(&plan_result.workflow),
                                        warnings: plan_result.warnings,
                                        workflow: Some(plan_result.workflow),
                                        error: None,
                                    }
                                }
                                Err(e) => {
                                    eprintln!(
                                        "\r  [error] turn {}: {:#}                    ",
                                        turn_idx + 1,
                                        e
                                    );
                                    TurnResult::error(e.to_string())
                                }
                            }
                        } else {
                            // Turn 2+: patch existing workflow
                            let Some(ref wf) = current_workflow else {
                                turn_results.push(TurnResult::error(
                                    "no workflow from previous turn".into(),
                                ));
                                continue;
                            };
                            match patch_workflow_with_backend(
                                llm.as_ref(),
                                wf,
                                &turn.prompt,
                                &tools,
                                false,
                                false,
                            )
                            .await
                            {
                                Ok(patch_result) => {
                                    let patched = apply_patch(wf, &patch_result);
                                    let tr = TurnResult {
                                        valid: true,
                                        node_count: patched.nodes.len(),
                                        tools_found: sorted_tools(&patched),
                                        patterns_found: sorted_patterns(&patched),
                                        warnings: patch_result.warnings,
                                        workflow: Some(patched.clone()),
                                        error: None,
                                    };
                                    current_workflow = Some(patched);
                                    tr
                                }
                                Err(e) => {
                                    eprintln!(
                                        "\r  [error] turn {}: {:#}                    ",
                                        turn_idx + 1,
                                        e
                                    );
                                    TurnResult::error(e.to_string())
                                }
                            }
                        };
                        turn_results.push(turn_result);
                    }

                    prog.tick();
                    RunResult {
                        endpoint,
                        turn_results,
                    }
                }),
            ));
        }
    }

    // Collect results back into per-case groups
    let mut case_runs: Vec<Vec<RunResult>> = cases.iter().map(|_| Vec::new()).collect();
    for (case_idx, _run_idx, handle) in handles {
        let result = handle.await.context("eval task panicked")?;
        case_runs[case_idx].push(result);
    }

    progress.finish();
    println!();

    // Score and print
    let mut all_results = Vec::new();
    let mut total_passed = 0u32;
    let mut total_runs = 0u32;

    for (case, runs) in cases.iter().zip(case_runs.iter()) {
        let num_turns = case.turns.len();
        let mut case_passed = 0u32;

        for run in runs {
            let all_turns_pass = case.turns.iter().enumerate().all(|(ti, turn)| {
                if let Some(ref expect) = turn.expect {
                    run.turn_results
                        .get(ti)
                        .and_then(|tr| {
                            if tr.valid {
                                tr.workflow.as_ref().map(|wf| score_run(wf, expect))
                            } else {
                                Some(false)
                            }
                        })
                        .unwrap_or(false)
                } else {
                    true // no expectations = pass
                }
            });
            if all_turns_pass {
                case_passed += 1;
            }
        }

        total_passed += case_passed;
        total_runs += runs_per_case;

        // Print scorecard line
        if num_turns == 1 {
            let node_counts: Vec<String> = runs
                .iter()
                .map(|r| r.turn_results[0].node_count.to_string())
                .collect();
            let expect = case.turns[0].expect.as_ref();
            let tools_ok = expect.is_none_or(|exp| {
                exp.required_tools.is_empty()
                    || runs.iter().all(|r| {
                        exp.required_tools
                            .iter()
                            .all(|t| r.turn_results[0].tools_found.contains(t))
                    })
            });
            let patterns_ok = expect.is_none_or(|exp| {
                exp.required_patterns.is_empty()
                    || runs.iter().all(|r| {
                        exp.required_patterns
                            .iter()
                            .all(|p| r.turn_results[0].patterns_found.contains(p))
                    })
            });
            let patterns_status = match expect {
                Some(exp) if !exp.required_patterns.is_empty() => {
                    if patterns_ok {
                        "ok"
                    } else {
                        "MISSING"
                    }
                }
                _ => "-",
            };
            println!(
                "  {:<35} {}/{} pass  nodes: {}  tools: {}  patterns: {}",
                case.name,
                case_passed,
                runs_per_case,
                node_counts.join(","),
                if tools_ok { "ok" } else { "MISSING" },
                patterns_status,
            );
        } else {
            let turn_summaries: Vec<String> = (0..num_turns)
                .map(|ti| {
                    let counts: Vec<String> = runs
                        .iter()
                        .map(|r| {
                            r.turn_results
                                .get(ti)
                                .map_or("x".into(), |tr| tr.node_count.to_string())
                        })
                        .collect();
                    format!("T{}: {}", ti + 1, counts.join(","))
                })
                .collect();
            println!(
                "  {:<35} {}/{} pass  {}",
                case.name,
                case_passed,
                runs_per_case,
                turn_summaries.join("  "),
            );
        }

        all_results.push(serde_json::json!({
            "name": case.name,
            "turns": num_turns,
            "runs": runs,
            "score": {
                "pass_rate": format!("{}/{}", case_passed, runs_per_case),
                "all_valid": runs.iter().all(|r| r.turn_results.iter().all(|tr| tr.valid)),
                "structure_match": case_passed == runs_per_case,
            }
        }));
    }

    let pass_pct = if total_runs > 0 {
        (total_passed as f64 * 100.0 / total_runs as f64).round() as u32
    } else {
        0
    };
    println!("\n  Overall: {total_passed}/{total_runs} ({pass_pct}%)");

    write_results(
        &crate_root,
        &EvalSummary {
            model,
            prompt_path: &prompt_rel,
            prompt_hash: &prompt_hash,
            runs_per_case,
            total_passed,
            total_runs,
        },
        &all_results,
    )?;

    Ok(())
}
