import { commands } from "../bindings";
import type {
  HydratedRunTrace,
  HydratedTraceStep,
  HydratedWorldModelDelta,
  HydratedTraceMilestone,
  HydratedTerminalFrame,
} from "../bindings";
import type {
  RunTrace,
  TraceStep,
  WorldModelDelta,
  TraceMilestone,
  TerminalFrame,
} from "./slices/assistantSlice";

interface LoadContext {
  projectPath: string | null;
  projectName: string;
  projectId: string;
  storeTraces: boolean;
}

function fromStep(s: HydratedTraceStep): TraceStep {
  return {
    stepIndex: s.step_index,
    toolName: s.tool_name,
    phase: s.phase,
    body: s.body,
    failed: s.failed,
  };
}

function fromDelta(d: HydratedWorldModelDelta): WorldModelDelta {
  return {
    stepIndex: d.step_index,
    changedFields: d.changed_fields,
  };
}

function fromMilestone(m: HydratedTraceMilestone): TraceMilestone {
  return {
    stepIndex: m.step_index,
    kind: m.kind,
    text: m.text,
  };
}

function fromTerminal(f: HydratedTerminalFrame): TerminalFrame {
  return {
    kind: f.kind,
    detail: f.detail,
  };
}

/**
 * Load the latest run's trace from disk. Returns `null` when the
 * project has no recorded runs, the persistence kill switch is off,
 * or the events file is empty/malformed.
 */
export async function loadLatestRunTrace(
  ctx: LoadContext,
): Promise<RunTrace | null> {
  try {
    const res = await commands.loadLatestRunTrace({
      project_path: ctx.projectPath,
      project_name: ctx.projectName,
      project_id: ctx.projectId,
      store_traces: ctx.storeTraces,
    });
    if (res.status !== "ok" || res.data == null) return null;
    const h: HydratedRunTrace = res.data;
    return {
      runId: h.run_id,
      phase: h.phase,
      activeSubgoal: h.active_subgoal,
      steps: h.steps.map(fromStep),
      worldModelDeltas: h.world_model_deltas.map(fromDelta),
      milestones: h.milestones.map(fromMilestone),
      terminalFrame: h.terminal_frame ? fromTerminal(h.terminal_frame) : null,
    };
  } catch {
    return null;
  }
}
