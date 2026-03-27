/** Color palette for output field types, used in output port dots and data edges. */
export const TYPE_COLORS: Record<string, string> = {
  Bool:   "#10b981", // green
  Number: "#3b82f6", // blue
  String: "#9ca3af", // light gray
  Array:  "#a855f7", // purple
  Object: "#f59e0b", // orange
  Any:    "#6b7280", // dim gray
};

export function typeColor(fieldType: string): string {
  return TYPE_COLORS[fieldType] ?? TYPE_COLORS.Any;
}
