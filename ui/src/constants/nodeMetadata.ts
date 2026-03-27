/** Shared node metadata for colors and short icon labels used in the graph canvas and walkthrough panel. */
export const nodeMetadata: Record<string, { color: string; icon: string }> = {
  // Native — Query
  FindText:        { color: "#a855f7", icon: "FT" },
  FindImage:       { color: "#a855f7", icon: "FI" },
  FindApp:         { color: "#a855f7", icon: "FA" },
  TakeScreenshot:  { color: "#a855f7", icon: "SS" },
  // Native — Action
  Click:           { color: "#f59e0b", icon: "CK" },
  Hover:           { color: "#f59e0b", icon: "HV" },
  Drag:            { color: "#f59e0b", icon: "DG" },
  TypeText:        { color: "#f59e0b", icon: "TT" },
  PressKey:        { color: "#f59e0b", icon: "PK" },
  Scroll:          { color: "#f59e0b", icon: "SC" },
  FocusWindow:     { color: "#50c878", icon: "FW" },
  LaunchApp:       { color: "#50c878", icon: "LA" },
  QuitApp:         { color: "#50c878", icon: "QA" },
  // CDP — Query
  CdpWait:         { color: "#60a5fa", icon: "CW" },
  // CDP — Action
  CdpClick:        { color: "#3b82f6", icon: "CC" },
  CdpHover:        { color: "#3b82f6", icon: "CH" },
  CdpFill:         { color: "#3b82f6", icon: "CF" },
  CdpType:         { color: "#3b82f6", icon: "CT" },
  CdpPressKey:     { color: "#3b82f6", icon: "CP" },
  CdpNavigate:     { color: "#3b82f6", icon: "CN" },
  CdpNewPage:      { color: "#3b82f6", icon: "NP" },
  CdpClosePage:    { color: "#3b82f6", icon: "XP" },
  CdpSelectPage:   { color: "#3b82f6", icon: "SP" },
  CdpHandleDialog: { color: "#3b82f6", icon: "HD" },
  // AI
  AiStep:          { color: "#4c9ee8", icon: "AI" },
  // Control Flow
  If:              { color: "#10b981", icon: "IF" },
  Switch:          { color: "#10b981", icon: "SW" },
  Loop:            { color: "#10b981", icon: "LP" },
  EndLoop:         { color: "#10b981", icon: "EL" },
  // Generic
  McpToolCall:     { color: "#666",    icon: "MC" },
  AppDebugKitOp:   { color: "#ef4444", icon: "DK" },
};

export const defaultNodeMetadata = { color: "#666", icon: "??" };
