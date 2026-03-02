use anyhow::{Context, Result};
use clap::Parser;
use clickweave_core::{NodeType, Workflow};
use clickweave_llm::planner::plan_workflow_with_backend;
use clickweave_llm::{LlmClient, LlmConfig};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

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
    endpoint: String,
    model: String,
    #[serde(default)]
    api_key: Option<String>,
    #[serde(default)]
    temperature: Option<f32>,
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
    prompt: String,
    #[serde(default)]
    expect: CaseExpectations,
}

#[derive(Deserialize, Default)]
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
struct RunResult {
    valid: bool,
    node_count: usize,
    tools_found: Vec<String>,
    patterns_found: Vec<String>,
    warnings: Vec<String>,
    workflow: Option<Workflow>,
    error: Option<String>,
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
    }
    patterns
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

    // Create LLM client
    let llm = LlmClient::new(LlmConfig {
        base_url: config
            .llm
            .endpoint
            .trim_end_matches('/')
            .trim_end_matches("/chat/completions")
            .to_string(),
        api_key: config.llm.api_key.clone(),
        model: model.to_string(),
        temperature: config.llm.temperature,
        max_tokens: Some(4096),
        ..LlmConfig::default()
    });

    println!("planner-eval — model: {model}, prompt: {prompt_rel} ({prompt_hash})\n");

    let mut all_results = Vec::new();
    let mut total_passed = 0u32;
    let mut total_runs = 0u32;

    for case in &cases {
        let mut case_runs = Vec::new();
        let mut case_passed = 0u32;

        for _ in 0..runs_per_case {
            let run_result = match plan_workflow_with_backend(
                &llm,
                &case.prompt,
                &tools_json,
                false,
                false,
                Some(&prompt_template),
            )
            .await
            {
                Ok(plan_result) => {
                    if score_run(&plan_result.workflow, &case.expect) {
                        case_passed += 1;
                    }
                    let mut tools_found: Vec<_> =
                        extract_tools(&plan_result.workflow).into_iter().collect();
                    tools_found.sort();
                    let mut patterns_found: Vec<_> = extract_patterns(&plan_result.workflow)
                        .into_iter()
                        .collect();
                    patterns_found.sort();
                    RunResult {
                        valid: true,
                        node_count: plan_result.workflow.nodes.len(),
                        tools_found,
                        patterns_found,
                        warnings: plan_result.warnings,
                        workflow: Some(plan_result.workflow),
                        error: None,
                    }
                }
                Err(e) => RunResult {
                    valid: false,
                    node_count: 0,
                    tools_found: vec![],
                    patterns_found: vec![],
                    warnings: vec![],
                    workflow: None,
                    error: Some(e.to_string()),
                },
            };
            case_runs.push(run_result);
        }

        total_passed += case_passed;
        total_runs += runs_per_case;

        // Print scorecard line
        let node_counts: Vec<String> = case_runs.iter().map(|r| r.node_count.to_string()).collect();
        let tools_ok = case.expect.required_tools.is_empty()
            || case_runs.iter().all(|r| {
                case.expect
                    .required_tools
                    .iter()
                    .all(|t| r.tools_found.contains(t))
            });
        let patterns_ok = case.expect.required_patterns.is_empty()
            || case_runs.iter().all(|r| {
                case.expect
                    .required_patterns
                    .iter()
                    .all(|p| r.patterns_found.contains(p))
            });

        let patterns_status = if case.expect.required_patterns.is_empty() {
            "-"
        } else if patterns_ok {
            "ok"
        } else {
            "MISSING"
        };
        println!(
            "  {:<30} {}/{} pass  nodes: {}  tools: {}  patterns: {}",
            case.name,
            case_passed,
            runs_per_case,
            node_counts.join(","),
            if tools_ok { "ok" } else { "MISSING" },
            patterns_status,
        );

        all_results.push(serde_json::json!({
            "name": case.name,
            "runs": case_runs,
            "score": {
                "pass_rate": format!("{}/{}", case_passed, runs_per_case),
                "valid": case_runs.iter().all(|r| r.valid),
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
