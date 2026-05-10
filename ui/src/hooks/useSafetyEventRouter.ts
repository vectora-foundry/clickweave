/**
 * `useSafetyEventRouter` — mounted once at AppShell root.
 *
 * Subscribes to `executor://supervision_paused` and
 * `agent://approval_required`. Dispatches to the appropriate store slice
 * based on the `SafetyScope` discriminant:
 *
 * - `scope.kind === "skill"` → `setSectionApproval`: consumed by
 *   `SkillSectionApprovalOverlay` (inline overlay on the active step card).
 * - `scope.kind === "ad_hoc"` → `setChatAnchoredApproval`: consumed by
 *   `AssistantThread` (chat-anchored approval card).
 *
 * Skill identity is frozen at execution start (D8 freeze invariant), so
 * the router never has to handle a missing `skill_id` mid-run.
 */

import { useEffect } from "react";
import { listen } from "@tauri-apps/api/event";
import type {
  SafetyScope,
  SectionApprovalPause,
  ChatAnchoredApprovalPause,
} from "../store/slices/executionSlice";
import { useStore } from "../store/useAppStore";

interface SupervisionPausedPayload {
  scope: SafetyScope;
  finding: string;
  screenshot: string | null;
}

interface ApprovalRequiredPayload {
  /** run_id is included for staleness checks on the caller side. */
  run_id: string;
  scope: SafetyScope | null;
  tool_name: string;
  arguments: unknown;
  description: string;
}

export function useSafetyEventRouter() {
  useEffect(() => {
    const unlisteners: (() => void)[] = [];
    let cancelled = false;

    const sub = (p: Promise<() => void>) =>
      p
        .then((u) => {
          if (cancelled) {
            u();
            return;
          }
          unlisteners.push(u);
        })
        .catch((err) => {
          console.error("useSafetyEventRouter: listener setup failed:", err);
        });

    sub(
      listen<SupervisionPausedPayload>(
        "executor://supervision_paused",
        (e) => {
          const { scope, finding, screenshot } = e.payload;
          routeSupervisionPause(scope, finding, screenshot ?? null);
        },
      ),
    );

    sub(
      listen<ApprovalRequiredPayload>(
        "agent://approval_required",
        (e) => {
          const { scope, tool_name, description } = e.payload;
          if (!scope) return;
          routeApprovalRequired(scope, tool_name, description);
        },
      ),
    );

    return () => {
      cancelled = true;
      unlisteners.forEach((u) => u());
    };
  }, []);
}

function routeSupervisionPause(
  scope: SafetyScope,
  finding: string,
  screenshot: string | null,
) {
  const store = useStore.getState();
  if (scope.kind === "skill") {
    const pause: SectionApprovalPause = {
      scope: scope as Extract<SafetyScope, { kind: "skill" }>,
      finding,
      screenshot,
    };
    store.setSectionApproval(pause);
  } else {
    const pause: ChatAnchoredApprovalPause = {
      scope: scope as Extract<SafetyScope, { kind: "ad_hoc" }>,
      finding,
      screenshot,
    };
    store.setChatAnchoredApproval(pause);
  }
}

function routeApprovalRequired(
  scope: SafetyScope,
  toolName: string,
  description: string,
) {
  const store = useStore.getState();
  if (scope.kind === "skill") {
    const pause: SectionApprovalPause = {
      scope: scope as Extract<SafetyScope, { kind: "skill" }>,
      finding: `Approval required: ${toolName} — ${description}`,
      screenshot: null,
    };
    store.setSectionApproval(pause);
  } else {
    const pause: ChatAnchoredApprovalPause = {
      scope: scope as Extract<SafetyScope, { kind: "ad_hoc" }>,
      finding: `Approval required: ${toolName} — ${description}`,
      screenshot: null,
    };
    store.setChatAnchoredApproval(pause);
  }
}
