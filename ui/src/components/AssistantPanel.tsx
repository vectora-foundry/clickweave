import type { AssistantMessage } from "../store/slices/assistantSlice";
import { useHorizontalResize } from "../hooks/useHorizontalResize";
import { AssistantThread } from "./shell/AssistantThread";

/**
 * D21 — thin drawer wrapper. The body (intent bar, messages,
 * cards, run trace, composer) lives in `AssistantThread.tsx`. The
 * resize handle and drawer-only layout chrome stay here.
 *
 * D15: `AmbiguityResolutionModal` and `ConfirmClearConversationModal`
 * are mounted at `AppShell` root and read their open state from the
 * store — they are NOT mounted by this wrapper.
 */
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
  const { width, handleResizeStart } = useHorizontalResize();
  if (!open) return null;
  return (
    <div
      className="relative flex h-full flex-col border-l border-[var(--border)] bg-[var(--bg-panel)]"
      style={{ width, minWidth: width }}
    >
      <div
        onMouseDown={handleResizeStart}
        className="absolute left-0 top-0 z-10 h-full w-1.5 cursor-col-resize hover:bg-[var(--accent-coral)]/30 active:bg-[var(--accent-coral)]/40"
      />
      <AssistantThread
        error={error}
        messages={messages}
        onSendMessage={onSendMessage}
        showHeader={true}
        onCloseDrawer={onClose}
      />
    </div>
  );
}
