import { BaseEdge, type EdgeProps, getSmoothStepPath } from "@xyflow/react";
import { typeColor } from "../utils/typeColors";

export function DataEdge({
  sourceX,
  sourceY,
  targetX,
  targetY,
  sourcePosition,
  targetPosition,
  data,
}: EdgeProps) {
  const [edgePath] = getSmoothStepPath({
    sourceX,
    sourceY,
    targetX,
    targetY,
    sourcePosition,
    targetPosition,
  });
  const fieldType = (data as Record<string, unknown> | undefined)?.fieldType as string | undefined;
  const color = typeColor(fieldType ?? "Any");

  return (
    <BaseEdge
      path={edgePath}
      style={{
        stroke: color,
        strokeWidth: 1.5,
        strokeDasharray: "4 2",
        pointerEvents: "none",
        opacity: 0.6,
      }}
    />
  );
}
