import { useCallback, useMemo, useState } from "react";
import type { AppKind, Node } from "../../bindings";
import type { DetailTab } from "../../store/useAppStore";
import { RunsTab, SetupTab, TraceTab } from "./tabs";
import { OutputsSection } from "./OutputsSection";

interface NodeDetailModalProps {
  node: Node | null;
  /** All nodes in the workflow, used to compute consumers and nodeNames. */
  nodes: Node[];
  projectPath: string | null;
  workflowId: string;
  workflowName: string;
  tab: DetailTab;
  onTabChange: (tab: DetailTab) => void;
  onUpdate: (id: string, updates: Partial<Node>) => void;
  onClose: () => void;
  appKind?: AppKind;
}

const tabs: { key: DetailTab; label: string }[] = [
  { key: "setup", label: "Setup" },
  { key: "trace", label: "Trace" },
  { key: "runs", label: "Runs" },
];

export function NodeDetailModal({
  node,
  nodes,
  projectPath,
  workflowId,
  workflowName,
  tab,
  onTabChange,
  onUpdate,
  onClose,
  appKind,
}: NodeDetailModalProps) {
  const [selectedRunId, setSelectedRunId] = useState<string | null>(null);

  // Build auto_id -> display name map for all workflow nodes.
  const nodeNames = useMemo<Record<string, string>>(() => {
    const map: Record<string, string> = {};
    for (const n of nodes) {
      if (n.auto_id) {
        map[n.auto_id] = n.name;
      }
    }
    return map;
  }, [nodes]);

  // Build consumers map: for the selected node, which of its output fields
  // are consumed by which downstream nodes (by auto_id).
  const consumers = useMemo<Record<string, string[]>>(() => {
    return {};
  }, [node?.auto_id, nodes]);

  const nodeTypeName = node?.node_type.type;
  const isActionNode = useMemo(() => {
    if (!nodeTypeName) return false;
    // Action nodes are those without query output schemas
    const queryTypes = ["FindText", "FindImage", "FindApp", "TakeScreenshot", "CdpWait", "AiStep", "McpToolCall", "AppDebugKitOp"];
    return !queryTypes.includes(nodeTypeName);
  }, [nodeTypeName]);

  const handleEnableVerification = useCallback(() => {
    if (!node) return;
    // Action nodes all have optional verification_method/verification_assertion
    // fields, but the NodeType union doesn't expose them on every variant.
    const updated = {
      ...node.node_type,
      verification_method: "Vlm" as const,
      verification_assertion: "",
    } as typeof node.node_type;
    onUpdate(node.id, { node_type: updated });
  }, [node, onUpdate]);

  if (!node) return null;

  return (
    <div className="flex w-[420px] flex-shrink-0 flex-col border-l border-[var(--border)] bg-[var(--bg-panel)]">
        <div className="flex items-center justify-between border-b border-[var(--border)] px-4 py-3">
          <div className="flex items-center gap-2">
            <span className="text-sm font-semibold text-[var(--text-primary)]">
              {node.name}
            </span>
            <span className="text-xs text-[var(--text-muted)]">
              {node.node_type.type}
            </span>
            {node.auto_id && (
              <span className="text-xs font-mono text-[var(--text-muted)]">
                {node.auto_id}
              </span>
            )}
          </div>
          <button
            onClick={onClose}
            className="text-[var(--text-muted)] hover:text-[var(--text-primary)]"
          >
            x
          </button>
        </div>

        <div className="flex border-b border-[var(--border)]">
          {tabs.map((t) => (
            <button
              key={t.key}
              onClick={() => onTabChange(t.key)}
              className={`px-4 py-2 text-xs font-medium transition-colors ${
                tab === t.key
                  ? "border-b-2 border-[var(--accent-coral)] text-[var(--text-primary)]"
                  : "text-[var(--text-secondary)] hover:text-[var(--text-primary)]"
              }`}
            >
              {t.label}
            </button>
          ))}
        </div>

        <div className="flex-1 overflow-y-auto p-4">
          {tab === "setup" && (
            <>
              <SetupTab node={node} onUpdate={(u) => onUpdate(node.id, u)} projectPath={projectPath} appKind={appKind} />
              <OutputsSection
                nodeTypeName={node.node_type.type}
                nodeType={node.node_type as unknown as Record<string, unknown>}
                autoId={node.auto_id}
                consumers={consumers}
                isActionNode={isActionNode}
                onEnableVerification={isActionNode ? handleEnableVerification : undefined}
              />
            </>
          )}
          {tab === "trace" && (
            <TraceTab
              nodeName={node.name}
              projectPath={projectPath}
              workflowId={workflowId}
              workflowName={workflowName}
              initialRunId={selectedRunId}
            />
          )}
          {tab === "runs" && (
            <RunsTab
              nodeName={node.name}
              projectPath={projectPath}
              workflowId={workflowId}
              workflowName={workflowName}
              onSelectRun={(runId) => {
                setSelectedRunId(runId);
                onTabChange("trace");
              }}
            />
          )}
        </div>
    </div>
  );
}
