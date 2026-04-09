import { useState, useRef, useEffect } from "react";
import type { ChatEntry, WorkflowPatch } from "../bindings";
import { ChatMessage } from "./ChatMessage";
import { useHorizontalResize } from "../hooks/useHorizontalResize";
import { useStore } from "../store/useAppStore";

interface AssistantPanelProps {
  open: boolean;
  loading: boolean;
  retrying: boolean;
  error: string | null;
  messages: ChatEntry[];
  pendingPatch: WorkflowPatch | null;
  pendingPatchWarnings: string[];
  contextUsage: number | null;
  onSendMessage: (message: string) => void;
  onCancel: () => void;
  onApplyPatch: () => void;
  onDiscardPatch: () => void;
  onClearConversation: () => void;
  onClose: () => void;
}

export function AssistantPanel({
  open,
  loading,
  retrying,
  error,
  messages,
  pendingPatch,
  pendingPatchWarnings,
  contextUsage,
  onSendMessage,
  onCancel,
  onApplyPatch,
  onDiscardPatch,
  onClearConversation,
  onClose,
}: AssistantPanelProps) {
  const [input, setInput] = useState("");
  const { width, handleResizeStart } = useHorizontalResize();
  const messagesEndRef = useRef<HTMLDivElement>(null);
  const textareaRef = useRef<HTMLTextAreaElement>(null);

  // Auto-scroll to bottom when messages change
  useEffect(() => {
    messagesEndRef.current?.scrollIntoView({ behavior: "smooth" });
  }, [messages.length, loading]);

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
    if (!trimmed || loading) return;
    setInput("");
    onSendMessage(trimmed);
  };

  const handleKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      handleSend();
    }
  };

  // Find the index of the last assistant message
  const lastAssistantIndex = messages.reduceRight(
    (found, msg, idx) => (found === -1 && msg.role === "assistant" ? idx : found),
    -1,
  );

  const hasMessages = messages.length > 0;
  const intent = useStore((s) => s.workflow.intent);

  return (
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
          {contextUsage != null && (
            <span className={`text-[10px] ${contextUsage > 0.8 ? "text-amber-400" : "text-[var(--text-muted)]"}`}>
              Context: {Math.round(contextUsage * 100)}%
            </span>
          )}
        </div>
        <div className="flex items-center gap-1">
          {hasMessages && (
            <button
              onClick={onClearConversation}
              className="rounded px-2 py-1 text-[11px] text-[var(--text-muted)] hover:bg-[var(--bg-hover)] hover:text-[var(--text-secondary)]"
              title="Clear conversation"
            >
              Clear
            </button>
          )}
          <button
            onClick={onClose}
            className="rounded px-1.5 py-0.5 text-[var(--text-muted)] hover:bg-[var(--bg-hover)] hover:text-[var(--text-primary)]"
            title="Close panel"
          >
            &times;
          </button>
        </div>
      </div>

      {/* Intent display */}
      {intent && (
        <div className="flex items-center gap-1.5 border-b border-[var(--border)] px-4 py-1.5">
          <span className="text-[10px] font-medium text-[var(--text-muted)]">Intent:</span>
          <span className="text-[11px] text-[var(--text-secondary)] truncate">{intent}</span>
        </div>
      )}

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
            if (entry.role === "tool_call") {
              return (
                <div key={`${entry.timestamp}-${idx}`} className="px-3 py-1 text-[10px] text-[var(--text-muted)] font-mono">
                  &rarr; {entry.tool_name}
                </div>
              );
            }
            if (entry.role === "tool_result") {
              return (
                <div key={`${entry.timestamp}-${idx}`} className="px-3 py-1 text-[10px] text-[var(--text-muted)]">
                  &larr; {entry.content.slice(0, 100)}{entry.content.length > 100 ? "..." : ""}
                </div>
              );
            }
            return (
              <ChatMessage
                key={`${entry.timestamp}-${idx}`}
                entry={entry}
                isLastAssistant={idx === lastAssistantIndex}
                pendingPatch={pendingPatch}
                pendingPatchWarnings={pendingPatchWarnings}
                onApplyPatch={onApplyPatch}
                onDiscardPatch={onDiscardPatch}
              />
            );
          })}

          {/* Loading indicator */}
          {loading && (
            <div className="flex justify-start">
              <div className="flex items-center gap-2 rounded-lg bg-[var(--bg-hover)] px-3 py-2">
                <div className="h-3.5 w-3.5 animate-spin rounded-full border-2 border-[var(--accent-coral)] border-t-transparent" />
                <span className="text-xs text-[var(--text-secondary)]">
                  {retrying ? "Improving..." : "Thinking..."}
                </span>
              </div>
            </div>
          )}

          <div ref={messagesEndRef} />
        </div>
      </div>

      {/* Error */}
      {error && (
        <div className="mx-3 mb-2 rounded border border-red-500/30 bg-red-500/10 px-3 py-2 text-[11px] text-red-400">
          {error}
        </div>
      )}

      {/* Input */}
      <div className="border-t border-[var(--border)] px-3 py-3">
        <div className="flex gap-2">
          <textarea
            ref={textareaRef}
            value={input}
            onChange={(e) => setInput(e.target.value)}
            onKeyDown={handleKeyDown}
            placeholder="Ask about your workflow..."
            rows={2}
            disabled={loading}
            className="flex-1 resize-none rounded-lg border border-[var(--border)] bg-[var(--bg-input)] px-3 py-2 text-sm text-[var(--text-primary)] placeholder:text-[var(--text-muted)] outline-none focus:border-[var(--accent-coral)]"
          />
          {loading ? (
            <button
              onClick={onCancel}
              className="self-end rounded-lg border border-[var(--text-muted)] px-3 py-2 text-xs font-medium text-[var(--text-secondary)] hover:border-[var(--text-primary)] hover:text-[var(--text-primary)]"
              title="Stop request"
            >
              Stop
            </button>
          ) : (
            <button
              onClick={handleSend}
              disabled={!input.trim()}
              className="self-end rounded-lg bg-[var(--accent-coral)] px-3 py-2 text-xs font-medium text-white hover:opacity-90 disabled:opacity-40"
            >
              Send
            </button>
          )}
        </div>
        <p className="mt-1.5 text-[10px] text-[var(--text-muted)]">
          Enter to send, Shift+Enter for new line
        </p>
      </div>
    </div>
  );
}
