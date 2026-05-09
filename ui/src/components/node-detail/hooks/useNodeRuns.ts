import { useEffect, useState } from "react";
import { commands } from "../../../bindings";
import type { NodeRun } from "../../../bindings";

export function useNodeRuns(
  projectPath: string | null,
  projectId: string,
  projectName: string,
  nodeName: string,
): NodeRun[] {
  const [runs, setRuns] = useState<NodeRun[]>([]);

  useEffect(() => {
    commands
      .listRuns({
        project_path: projectPath,
        project_id: projectId,
        project_name: projectName,
        node_name: nodeName,
      })
      .then((result) => {
        if (result.status === "ok") {
          setRuns([...result.data].reverse());
        }
      });
  }, [projectPath, projectId, projectName, nodeName]);

  return runs;
}
