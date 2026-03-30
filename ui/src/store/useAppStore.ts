import { create } from "zustand";
import { createAssistantSlice } from "./slices/assistantSlice";
import { createExecutionSlice } from "./slices/executionSlice";
import { createHistorySlice } from "./slices/historySlice";
import { createLogSlice } from "./slices/logSlice";
import { createPlannerSlice } from "./slices/plannerSlice";
import { createProjectSlice } from "./slices/projectSlice";
import { createSettingsSlice } from "./slices/settingsSlice";
import { createUiSlice } from "./slices/uiSlice";
import { createVerdictSlice } from "./slices/verdictSlice";
import { createWalkthroughSlice } from "./slices/walkthroughSlice";
import type { StoreState } from "./slices/types";

export type { DetailTab, EndpointConfig } from "./state";

// ── Zustand store ────────────────────────────────────────────────

export const useStore = create<StoreState>()((...a) => ({
  ...createSettingsSlice(...a),
  ...createProjectSlice(...a),
  ...createAssistantSlice(...a),
  ...createExecutionSlice(...a),
  ...createHistorySlice(...a),
  ...createLogSlice(...a),
  ...createPlannerSlice(...a),
  ...createUiSlice(...a),
  ...createVerdictSlice(...a),
  ...createWalkthroughSlice(...a),
}));
