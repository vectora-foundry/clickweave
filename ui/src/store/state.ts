export type DetailTab = "setup" | "trace" | "runs";

export interface EndpointConfig {
  baseUrl: string;
  apiKey: string;
  model: string;
}

export const DEFAULT_ENDPOINT: EndpointConfig = {
  baseUrl: "http://localhost:1234/v1",
  apiKey: "",
  model: "local",
};

export const DEFAULT_FAST_ENABLED = false;

export type PermissionLevel = "ask" | "allow" | "deny";

/** A single pattern-based permission rule. */
export interface PermissionRule {
  /** Glob pattern matched against the tool name (`*`, `?`). */
  toolPattern: string;
  /** Optional substring matched against the JSON-serialized arguments. */
  argsPattern?: string;
  action: PermissionLevel;
}

export interface ToolPermissions {
  allowAll: boolean;
  /** Per-tool 3-tier overrides. `ask` is the implicit default. */
  tools: Record<string, PermissionLevel>;
  /** Pattern rules evaluated alongside per-tool overrides. */
  patternRules: PermissionRule[];
  /**
   * When true, destructive tools prompt even if they (or the global
   * override) are set to `allow`. Default true.
   */
  requireConfirmDestructive: boolean;
  /**
   * Halt the run after this many consecutive destructive tool calls.
   * `0` disables the cap. Default 3.
   */
  consecutiveDestructiveCap: number;
}

export const DEFAULT_TOOL_PERMISSIONS: ToolPermissions = {
  allowAll: false,
  tools: {},
  patternRules: [],
  requireConfirmDestructive: true,
  consecutiveDestructiveCap: 3,
};

/**
 * Mirrors `clickweave_core::project::PROJECT_SCHEMA_VERSION` (D33).
 * Bump in lock-step with the Rust constant whenever the manifest
 * shape changes.
 */
export const PROJECT_SCHEMA_VERSION = 1;
