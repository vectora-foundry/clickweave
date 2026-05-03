use super::*;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalScenario {
    pub id: String,
    pub description: String,
    pub goal: String,
    pub max_steps: usize,
    pub tools: Vec<ToolSpec>,
    #[serde(default)]
    pub tool_behaviors: Vec<ToolBehavior>,
    pub scoring: ScoringSpec,
}

impl EvalScenario {
    pub fn load(path: &Path) -> Result<Self> {
        let raw = fs::read_to_string(path).context("read scenario file")?;
        let scenario: Self = serde_json::from_str(&raw).context("parse scenario file")?;
        scenario.validate_privacy()?;
        Ok(scenario)
    }

    /// Fail closed on obvious private-data hazards. Evals should use
    /// synthetic fixtures; real user traces must be reduced/redacted before
    /// becoming fixtures.
    pub fn validate_privacy(&self) -> Result<()> {
        if !self.id.starts_with("synthetic_") {
            bail!(
                "scenario {} is not marked as synthetic; eval fixtures must use synthetic redacted data",
                self.id
            );
        }
        let raw = serde_json::to_string(self)?;
        if private_marker(&raw).is_some() {
            bail!(
                "scenario {} appears to contain private path/secret material; use a synthetic redacted fixture",
                self.id
            );
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub parameters: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolBehavior {
    pub tool: String,
    #[serde(default)]
    pub response: Option<Value>,
    #[serde(default)]
    pub error: bool,
    #[serde(default)]
    pub required_args: Vec<String>,
    #[serde(default)]
    pub requires_state: HashMap<String, Value>,
    #[serde(default)]
    pub sets_state: HashMap<String, Value>,
    #[serde(default)]
    pub response_sequence: Vec<ToolResponse>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ToolResponse {
    #[serde(default)]
    pub response: Option<Value>,
    #[serde(default)]
    pub error: bool,
    #[serde(default)]
    pub sets_state: HashMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScoringSpec {
    #[serde(default)]
    pub required_tools: Vec<String>,
    #[serde(default)]
    pub forbidden_tools: Vec<String>,
    #[serde(default)]
    pub allowed_error_tools: Vec<String>,
    #[serde(default)]
    pub required_agent_tools: Vec<String>,
    #[serde(default)]
    pub required_agent_tool_groups: Vec<Vec<String>>,
    #[serde(default)]
    pub required_agent_tool_counts: HashMap<String, usize>,
    #[serde(default)]
    pub forbidden_agent_tools: Vec<String>,
    #[serde(default)]
    pub stop_after_agent_tools: Vec<String>,
    #[serde(default)]
    pub max_agent_tool_calls: Option<usize>,
    #[serde(default)]
    pub max_repeated_action_warnings: Option<usize>,
    #[serde(default = "default_true")]
    pub completion_required: bool,
}

fn default_true() -> bool {
    true
}

pub fn load_scenarios_dir(path: &Path) -> Result<Vec<EvalScenario>> {
    let mut files: Vec<PathBuf> = fs::read_dir(path)
        .with_context(|| format!("read scenario dir {}", path.display()))?
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let path = entry.path();
            (path.extension().and_then(|ext| ext.to_str()) == Some("json")).then_some(path)
        })
        .collect();
    files.sort();

    let mut scenarios = Vec::with_capacity(files.len());
    for file in files {
        scenarios.push(EvalScenario::load(&file)?);
    }
    Ok(scenarios)
}
