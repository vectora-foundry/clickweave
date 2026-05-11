import type { AgentSlice } from "./agentSlice";
import type { AssistantSlice } from "./assistantSlice";
import type { ExecutionSlice } from "./executionSlice";
import type { LogSlice } from "./logSlice";
import type { ProjectSlice } from "./projectSlice";
import type { SettingsSlice } from "./settingsSlice";
import type { SkillsSlice } from "./skillsSlice";
import type { UiSlice } from "./uiSlice";
import type { VerdictSlice } from "./verdictSlice";
import type { WalkthroughSlice } from "./walkthroughSlice";

export type StoreState = AgentSlice &
  AssistantSlice &
  ExecutionSlice &
  LogSlice &
  ProjectSlice &
  SettingsSlice &
  SkillsSlice &
  UiSlice &
  VerdictSlice &
  WalkthroughSlice;
