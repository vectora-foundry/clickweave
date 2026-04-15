import { useState, useRef, useEffect } from "react";
import type { AssistantMessage } from "../store/slices/assistantSlice";
import { useHorizontalResize } from "../hooks/useHorizontalResize";
import { useStore } from "../store/useAppStore";
import { AmbiguityResolutionCard } from "./AmbiguityResolutionCard";
import { AmbiguityResolutionModal } from "./AmbiguityResolutionModal";

interface AssistantPanelProps {
  open: boolean;
  error: string | null;
  messages: AssistantMessage[];
  onSendMessage: (message: string) => void;
  onClose: () => void;
}

export function AssistantPanel({
  open,
  error,
  messages,
  onSendMessage,
  onClose,
}: AssistantPanelProps) {
  const [input, setInput] = useState("");
  const { width, handleResizeStart } = useHorizontalResize();
  const messagesEndRef = useRef<HTMLDivElement>(null);
  const textareaRef = useRef<HTMLTextAreaElement>(null);

  const agentStatus = useStore((s) => s.agentStatus);
  const pendingApproval = useStore((s) => s.pendingApproval);
  const stopAgent = useStore((s) => s.stopAgent);
  const approveAction = useStore((s) => s.approveAction);
  const rejectAction = useStore((s) => s.rejectAction);
  const ambiguityResolutions = useStore((s) => s.ambiguityResolutions);
  const activeAmbiguityId = useStore((s) => s.activeAmbiguityId);
  const openAmbiguityModal = useStore((s) => s.openAmbiguityModal);
  const closeAmbiguityModal = useStore((s) => s.closeAmbiguityModal);
  const activeAmbiguity =
    ambiguityResolutions.find((r) => r.id === activeAmbiguityId) ?? null;
  const agentRunning = agentStatus === "running";

  // Auto-scroll to bottom when messages change
  useEffect(() => {
    messagesEndRef.current?.scrollIntoView({ behavior: "smooth" });
  }, [messages.length]);

  // Focus textarea when panel opens
  useEffect(() => {
    if (open) {
      // Small delay to allow transition
      const timer = setTimeout(() => textareaRef.current?.focus(), 100);
      return () => clearTimeout(timer);
    }
  }, [open]);

  if (!open) return null;

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
    <>
    <div className="relative flex h-full flex-col border-l border-[var(--border)] bg-[var(--bg-panel)]" style={{ width, minWidth: width }}>
      {/* Resize handle */}
      <div
        onMouseDown={handleResizeStart}
        className="absolute left-0 top-0 z-10 h-full w-1.5 cursor-col-resize hover:bg-[var(--accent-coral)]/30 active:bg-[var(--accent-coral)]/40"
      />
      {/* Header */}
      <div className="flex items-center justify-between border-b border-[var(--border)] px-4 py-2.5">
        <div className="flex items-center gap-2">
          <h2 className="text-sm font-medium text-[var(--text-primary)]">
            Assistant
          </h2>
        </div>
        <div className="flex items-center gap-1">
          <button
            onClick={onClose}
            className="rounded px-1.5 py-0.5 text-[var(--text-muted)] hover:bg-[var(--bg-hover)] hover:text-[var(--text-primary)]"
            title="Close panel"
          >
            &times;
          </button>
        </div>
      </div>

      {/* Intent — editable inline */}
      <IntentBar />

      {/* Messages */}
      <div className="flex-1 overflow-y-auto px-3 py-3">
        {!hasMessages && (
          <div className="flex h-full items-center justify-center">
            <p className="text-center text-xs text-[var(--text-muted)]">
              Ask me to create or modify your workflow.
            </p>
          </div>
        )}

        <div className="space-y-3">
          {messages.map((entry, idx) => {
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

      {/* Approval card */}
      {pendingApproval && (
        <div className="mx-3 mb-2 rounded-lg border border-amber-500/40 bg-amber-500/10 px-3 py-2.5">
          <p className="text-[11px] font-medium text-amber-300 mb-1">
            Agent wants to execute:
          </p>
          <p className="text-xs text-[var(--text-primary)] font-mono mb-2 break-all">
            {pendingApproval.toolName}
            <span className="text-[var(--text-muted)]">
              ({typeof pendingApproval.arguments === "string"
                ? pendingApproval.arguments
                : JSON.stringify(pendingApproval.arguments, null, 0)?.slice(0, 120)}
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

      {/* Input */}
      <div className="border-t border-[var(--border)] px-3 py-3">
        {agentRunning ? (
          <div className="flex items-center justify-between">
            <div className="flex items-center gap-2">
              <div className="h-3 w-3 animate-spin rounded-full border-2 border-[var(--accent-coral)] border-t-transparent" />
              <span className="text-xs text-[var(--text-secondary)]">
                Agent running...
              </span>
            </div>
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
    {activeAmbiguity && (
      <AmbiguityResolutionModal
        resolution={activeAmbiguity}
        onClose={closeAmbiguityModal}
      />
    )}
    </>
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

  const displayIntent = workflowIntent;

  const startEdit = () => {
    if (isRunning) return;
    setDraft(displayIntent ?? "");
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
        <span className="text-[10px] font-medium text-[var(--text-muted)] shrink-0">Intent:</span>
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
      <span className="text-[10px] font-medium text-[var(--text-muted)] shrink-0">Intent:</span>
      <span className="text-[11px] text-[var(--text-secondary)] truncate">
        {displayIntent || "Click to set intent for outcome verification..."}
      </span>
    </button>
  );
}
