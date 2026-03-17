import type { AssistantSlice } from "./assistantSlice";
import type { ExecutionSlice } from "./executionSlice";
import type { HistorySlice } from "./historySlice";
import type { LogSlice } from "./logSlice";
import type { ProjectSlice } from "./projectSlice";
import type { SettingsSlice } from "./settingsSlice";
import type { UiSlice } from "./uiSlice";
import type { VerdictSlice } from "./verdictSlice";
import type { WalkthroughSlice } from "./walkthroughSlice";
import type { WalkthroughRecordingSlice } from "./walkthroughRecordingSlice";
import type { WalkthroughReviewSlice } from "./walkthroughReviewSlice";

export type StoreState = AssistantSlice &
  ExecutionSlice &
  HistorySlice &
  LogSlice &
  ProjectSlice &
  SettingsSlice &
  UiSlice &
  VerdictSlice &
  WalkthroughSlice &
  WalkthroughRecordingSlice &
  WalkthroughReviewSlice;
