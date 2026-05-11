import { create } from "zustand";
import { createAgentSlice } from "./slices/agentSlice";
import { createAssistantSlice } from "./slices/assistantSlice";
import { createExecutionSlice } from "./slices/executionSlice";
import { createLogSlice } from "./slices/logSlice";
import { createProjectSlice } from "./slices/projectSlice";
import { createSettingsSlice } from "./slices/settingsSlice";
import { createSkillsSlice } from "./slices/skillsSlice";
import { createUiSlice } from "./slices/uiSlice";
import { createVerdictSlice } from "./slices/verdictSlice";
import { createWalkthroughSlice } from "./slices/walkthroughSlice";
import type { StoreState } from "./slices/types";

export type { DetailTab, EndpointConfig } from "./state";

// ── Zustand store ────────────────────────────────────────────────

export const useStore = create<StoreState>()((...a) => ({
  ...createAgentSlice(...a),
  ...createSettingsSlice(...a),
  ...createProjectSlice(...a),
  ...createAssistantSlice(...a),
  ...createExecutionSlice(...a),
  ...createLogSlice(...a),
  ...createSkillsSlice(...a),
  ...createUiSlice(...a),
  ...createVerdictSlice(...a),
  ...createWalkthroughSlice(...a),
}));
