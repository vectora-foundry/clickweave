/** Shared group color palette used by CreateGroupPopover and other group UIs. */
export const GROUP_PALETTE = [
  "#6366f1",
  "#06b6d4",
  "#10b981",
  "#f59e0b",
  "#ef4444",
  "#ec4899",
] as const;

/** Convert hex color like "#6366f1" to "99, 102, 241" for rgba(). */
export function hexToRgb(hex: string): string {
  const r = parseInt(hex.slice(1, 3), 16);
  const g = parseInt(hex.slice(3, 5), 16);
  const b = parseInt(hex.slice(5, 7), 16);
  return `${r}, ${g}, ${b}`;
}
