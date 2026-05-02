import { useState, useRef, useEffect } from "react";
import type { AssistantMessage } from "../../store/slices/assistantSlice";
import { useStore } from "../../store/useAppStore";
import { AmbiguityResolutionCard } from "../AmbiguityResolutionCard";
import { isAgentActive } from "../../store/slices/agentSlice";
import { RunTraceView } from "../RunTraceView";

/**
 * D21 — body of the assistant conversation surface. Renders intent bar,
 * messages, error banner, the ambiguity / disagreement / destructive-cap /
 * approval cards, the live run trace, and the composer. Mounts NO modals —
 * the global `AmbiguityResolutionModal` and `ConfirmClearConversationModal`
 * are root-mounted by `AppShell` per D15.
 *
 * Two layout flags let the same body render in both surfaces:
 *  - `showHeader`: drawer header (Clear button + ×). Used by the
 *    `AssistantPanel` drawer wrapper.
 *  - `showClearIcon`: standalone trash icon at the body top. Used by
 *    `OverviewAssistantCard` per D14 — the Overview card has its own
 *    chrome and only needs a single Clear affordance inside the body.
 */
interface AssistantThreadProps {
  error: string | null;
  messages: AssistantMessage[];
  onSendMessage: (message: string) => void;
  /** Drawer chrome: Clear button + close × at top. Used by the legacy drawer wrapper. */
  showHeader: boolean;
  /** Standalone trash icon at the top of the body (Overview card uses this — D14). */
  showClearIcon?: boolean;
  /** Drawer wrapper's close handler. Required when `showHeader` is true. */
  onCloseDrawer?: () => void;
}

export function AssistantThread({
  error,
  messages,
  onSendMessage,
  showHeader,
  showClearIcon,
  onCloseDrawer,
}: AssistantThreadProps) {
  const [input, setInput] = useState("");
  const messagesEndRef = useRef<HTMLDivElement>(null);
  const textareaRef = useRef<HTMLTextAreaElement>(null);

  const agentStatus = useStore((s) => s.agentStatus);
  const activeRunId = useStore((s) => s.agentRunId);
  const pendingApproval = useStore((s) => s.pendingApproval);
  const completionDisagreement = useStore((s) => s.completionDisagreement);
  const consecutiveDestructiveCapHit = useStore(
    (s) => s.consecutiveDestructiveCapHit,
  );
  const setConsecutiveDestructiveCapHit = useStore(
    (s) => s.setConsecutiveDestructiveCapHit,
  );
  const confirmDisagreementAsComplete = useStore(
    (s) => s.confirmDisagreementAsComplete,
  );
  const cancelDisagreement = useStore((s) => s.cancelDisagreement);
  const stopAgent = useStore((s) => s.stopAgent);
  const approveAction = useStore((s) => s.approveAction);
  const rejectAction = useStore((s) => s.rejectAction);
  const ambiguityResolutions = useStore((s) => s.ambiguityResolutions);
  const openAmbiguityModal = useStore((s) => s.openAmbiguityModal);
  const setConfirmClearOpen = useStore((s) => s.setConfirmClearOpen);
  const agentNodeCount = useStore(
    (s) => s.workflow.nodes.filter((n) => n.source_run_id != null).length,
  );

  // Broader "active" check for features that must not race the
  // backend task — includes the VLM-disagreement resolver window.
  const agentActive = isAgentActive(agentStatus, completionDisagreement);
  const showClearAffordance =
    (messages.length > 0 || agentNodeCount > 0) && !agentActive;

  // Auto-scroll to bottom when messages change
  useEffect(() => {
    messagesEndRef.current?.scrollIntoView?.({ behavior: "smooth" });
  }, [messages.length]);

  // Focus textarea on mount
  useEffect(() => {
    const timer = setTimeout(() => textareaRef.current?.focus(), 100);
    return () => clearTimeout(timer);
  }, []);

  const handleSend = () => {
    const trimmed = input.trim();
    if (!trimmed) return;
    setInput("");
    onSendMessage(trimmed);
  };

  const handleKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      handleSend();
    }
  };

  const hasMessages = messages.length > 0;

  return (
    <div className="relative flex h-full min-h-0 flex-col overflow-hidden bg-[var(--bg-panel)]">
      {showHeader && (
        <div className="flex items-center justify-between border-b border-[var(--border)] px-4 py-2.5">
          <div className="flex items-center gap-2">
            <h2 className="text-sm font-medium text-[var(--text-primary)]">
              Assistant
            </h2>
          </div>
          <div className="flex items-center gap-1">
            {showClearAffordance && (
              <button
                onClick={() => setConfirmClearOpen(true)}
                className="rounded px-1.5 py-0.5 text-[11px] text-[var(--text-muted)] hover:bg-[var(--bg-hover)] hover:text-[var(--text-primary)]"
                title="Clear the conversation and remove agent-built nodes"
              >
                Clear
              </button>
            )}
            <button
              onClick={onCloseDrawer}
              className="rounded px-1.5 py-0.5 text-[var(--text-muted)] hover:bg-[var(--bg-hover)] hover:text-[var(--text-primary)]"
              title="Close panel"
            >
              &times;
            </button>
          </div>
        </div>
      )}

      {showClearIcon && showClearAffordance && (
        <div className="flex justify-end px-3 pt-2">
          <button
            onClick={() => setConfirmClearOpen(true)}
            aria-label="Clear conversation"
            title="Clear the conversation and remove agent-built nodes"
            className="rounded p-1 text-[var(--text-muted)] hover:bg-[var(--bg-hover)] hover:text-[var(--text-primary)]"
          >
            <svg
              width="12"
              height="12"
              viewBox="0 0 16 16"
              fill="none"
              stroke="currentColor"
              strokeWidth="1.3"
              strokeLinecap="round"
              strokeLinejoin="round"
            >
              <path d="M3 4h10M6 4V2h4v2M5 4l1 10h4l1-10" />
            </svg>
          </button>
        </div>
      )}

      {/* Intent — editable inline */}
      <IntentBar />

      <div className="min-h-0 flex-1 overflow-y-auto">
        {/* Messages */}
        <div className={`${hasMessages ? "" : "flex min-h-[120px] items-center justify-center"} px-3 py-3`}>
          {!hasMessages && (
            <p className="text-center text-xs text-[var(--text-muted)]">
              Ask me to create or modify your workflow.
            </p>
          )}

          <div className="space-y-3">
            {messages.map((entry, idx) => {
              if (entry.role === "system") {
                return (
                  <div
                    key={`${entry.timestamp}-${idx}`}
                    className="flex justify-center"
                  >
                    <div className="max-w-[70%] rounded border border-[var(--accent-blue)]/60 bg-[var(--accent-blue)]/10 px-2 py-1 text-center text-[11px] text-[var(--text-secondary)]">
                      {entry.content}
                    </div>
                  </div>
                );
              }
              const isUser = entry.role === "user";
              return (
                <div
                  key={`${entry.timestamp}-${idx}`}
                  className={`group flex flex-col ${isUser ? "items-end" : "items-start"}`}
                >
                  <div
                    className={`max-w-[85%] rounded-lg px-3 py-2 text-sm ${
                      isUser
                        ? "bg-[var(--accent-coral)]/15 text-[var(--text-primary)]"
                        : "bg-[var(--bg-hover)] text-[var(--text-primary)]"
                    }`}
                  >
                    <div className="whitespace-pre-wrap break-words leading-relaxed select-text">
                      {entry.content}
                    </div>
                  </div>
                </div>
              );
            })}

            <div ref={messagesEndRef} />
          </div>
        </div>

        {/* Error */}
        {error && (
          <div className="mx-3 mb-2 rounded border border-red-500/30 bg-red-500/10 px-3 py-2 text-[11px] text-red-400">
            {error}
          </div>
        )}

      {/* Ambiguity resolution cards — newest first, persists across runs. */}
      {ambiguityResolutions.map((r) => (
        <AmbiguityResolutionCard
          key={r.id}
          resolution={r}
          onOpen={() => openAmbiguityModal(r.id)}
        />
      ))}

      {/* VLM completion disagreement card.
          The backend halts the run when the post-agent_done VLM check
          rejects the agent's self-reported completion. Both buttons
          invoke `resolve_completion_disagreement` so the operator's
          decision is persisted to events.jsonl + the variant index and
          the final terminal `agent://complete` / `agent://stopped` event
          fires server-side. */}
      {completionDisagreement && (
        <div className="mx-3 mb-2 rounded-lg border border-orange-500/40 bg-orange-500/10 px-3 py-2.5">
          <p className="text-[11px] font-medium text-orange-300 mb-1">
            Completion check disagreed
          </p>
          <p className="text-[11px] text-[var(--text-secondary)] mb-2">
            Agent said: {completionDisagreement.agentSummary}
          </p>
          <img
            src={`data:image/jpeg;base64,${completionDisagreement.screenshotBase64}`}
            alt="Screenshot captured when the agent reported completion"
            className="mb-2 max-h-48 w-full rounded border border-[var(--border)] object-contain"
          />
          <p className="text-[11px] text-[var(--text-primary)] mb-2 whitespace-pre-wrap">
            VLM: {completionDisagreement.vlmReasoning}
          </p>
          <div className="flex gap-2">
            <button
              onClick={confirmDisagreementAsComplete}
              className="rounded-lg bg-green-600 px-3 py-1.5 text-xs font-medium text-white hover:bg-green-500"
            >
              Confirm complete
            </button>
            <button
              onClick={cancelDisagreement}
              className="rounded-lg border border-red-500/50 px-3 py-1.5 text-xs font-medium text-red-400 hover:bg-red-500/10"
            >
              Cancel run
            </button>
          </div>
        </div>
      )}

      {/* Consecutive-destructive cap hit notice */}
      {consecutiveDestructiveCapHit && (
        <div className="mx-3 mb-2 rounded-lg border border-red-500/40 bg-red-500/10 px-3 py-2.5">
          <p className="text-[11px] font-medium text-red-300 mb-1">
            Run halted: reached {consecutiveDestructiveCapHit.cap} consecutive
            destructive actions
          </p>
          <p className="text-[11px] text-[var(--text-secondary)] font-mono break-words mb-2">
            {consecutiveDestructiveCapHit.recentToolNames.join(", ")}
          </p>
          <button
            onClick={() => setConsecutiveDestructiveCapHit(null)}
            className="rounded-lg border border-[var(--border)] px-3 py-1.5 text-xs font-medium text-[var(--text-secondary)] hover:bg-[var(--bg-hover)]"
          >
            Dismiss
          </button>
        </div>
      )}

      {/* Approval card */}
      {pendingApproval && (
        <div className="mx-3 mb-2 rounded-lg border border-amber-500/40 bg-amber-500/10 px-3 py-2.5">
          <p className="text-[11px] font-medium text-amber-300 mb-1">
            Agent wants to execute:
          </p>
          <p className="text-xs text-[var(--text-primary)] font-mono mb-2 break-all">
            {pendingApproval.toolName}
            <span className="text-[var(--text-muted)]">
              (
              {typeof pendingApproval.arguments === "string"
                ? pendingApproval.arguments
                : JSON.stringify(pendingApproval.arguments, null, 0)?.slice(
                    0,
                    120,
                  )}
              )
            </span>
          </p>
          <div className="flex gap-2">
            <button
              onClick={approveAction}
              className="rounded-lg bg-green-600 px-3 py-1.5 text-xs font-medium text-white hover:bg-green-500"
            >
              Approve
            </button>
            <button
              onClick={rejectAction}
              className="rounded-lg border border-[var(--border)] px-3 py-1.5 text-xs font-medium text-[var(--text-secondary)] hover:bg-[var(--bg-hover)]"
            >
              Skip
            </button>
            <button
              onClick={stopAgent}
              className="rounded-lg border border-red-500/50 px-3 py-1.5 text-xs font-medium text-red-400 hover:bg-red-500/10"
            >
              Stop
            </button>
          </div>
        </div>
      )}

        {agentActive && activeRunId && <RunTraceView runId={activeRunId} />}
      </div>

      {/* Input — hidden while the agent is running OR while a VLM
          completion-disagreement resolver is pending. In the
          disagreement window the backend task is still alive and
          owns the workflow's cache/variant-index writes, so a new
          `startAgent` would race its final writes. The resolver's
          Confirm/Cancel buttons live in the disagreement card
          above. */}
      <div className="border-t border-[var(--border)] px-3 py-3">
        {agentActive ? (
          <div className="flex justify-end">
            <button
              onClick={stopAgent}
              className="rounded-lg border border-red-500/50 px-3 py-1.5 text-xs font-medium text-red-400 hover:bg-red-500/10 hover:text-red-300"
              title="Stop agent"
            >
              Stop
            </button>
          </div>
        ) : (
          <>
            <div className="flex gap-2">
              <textarea
                ref={textareaRef}
                value={input}
                onChange={(e) => setInput(e.target.value)}
                onKeyDown={handleKeyDown}
                placeholder="Ask about your workflow..."
                rows={2}
                className="flex-1 resize-none rounded-lg border border-[var(--border)] bg-[var(--bg-input)] px-3 py-2 text-sm text-[var(--text-primary)] placeholder:text-[var(--text-muted)] outline-none focus:border-[var(--accent-coral)]"
              />
              <button
                onClick={handleSend}
                disabled={!input.trim()}
                className="self-end rounded-lg bg-[var(--accent-coral)] px-3 py-2 text-xs font-medium text-white hover:opacity-90 disabled:opacity-40"
              >
                Send
              </button>
            </div>
            <p className="mt-1.5 text-[10px] text-[var(--text-muted)]">
              Enter to send, Shift+Enter for new line
            </p>
          </>
        )}
      </div>
    </div>
  );
}

function IntentBar() {
  const workflowIntent = useStore((s) => s.workflow.intent);
  const setIntent = useStore((s) => s.setIntent);
  const isRunning = useStore((s) => s.executorState === "running");
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState("");

  // Close editor when execution starts
  useEffect(() => {
    if (isRunning && editing) setEditing(false);
  }, [isRunning, editing]);

  const startEdit = () => {
    if (isRunning) return;
    setDraft(workflowIntent ?? "");
    setEditing(true);
  };

  const commit = () => {
    const value = draft.trim() || null;
    setIntent(value);
    setEditing(false);
  };

  const cancel = () => setEditing(false);

  if (editing) {
    return (
      <div className="flex items-center gap-1 border-b border-[var(--border)] px-4 py-1.5">
        <span className="text-[10px] font-medium text-[var(--text-muted)] shrink-0">
          Intent:
        </span>
        <input
          autoFocus
          className="flex-1 bg-transparent text-[11px] text-[var(--text-primary)] outline-none border-b border-[var(--accent-blue)]"
          value={draft}
          onChange={(e) => setDraft(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") commit();
            if (e.key === "Escape") cancel();
          }}
          onBlur={commit}
          placeholder="Describe what this workflow should accomplish..."
        />
      </div>
    );
  }

  return (
    <button
      onClick={startEdit}
      disabled={isRunning}
      className={`flex items-center gap-1.5 border-b border-[var(--border)] px-4 py-1.5 w-full text-left transition-colors ${isRunning ? "opacity-50 cursor-default" : "hover:bg-[var(--bg-hover)]"}`}
    >
      <span className="text-[10px] font-medium text-[var(--text-muted)] shrink-0">
        Intent:
      </span>
      <span className="text-[11px] text-[var(--text-secondary)] truncate">
        {workflowIntent || "Click to set intent for outcome verification..."}
      </span>
    </button>
  );
}
