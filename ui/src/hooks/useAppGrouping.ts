import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { Workflow } from "../bindings";
import { buildAppNameMap, computeAppMembers } from "../utils/appGroupComputation";
import { GROUP_COLORS, hashAppName } from "../utils/walkthroughGrouping";

export interface AppGroupMeta {
  appName: string;
  color: string;
  anchorId: string;
}

export function useAppGrouping(workflow: Workflow) {
  const [collapsedApps, setCollapsedApps] = useState<Set<string>>(new Set());

  const appNameMap = useMemo(() => buildAppNameMap(workflow), [workflow.nodes, workflow.edges]);

  const appGroups = useMemo(
    () => computeAppMembers(workflow, appNameMap),
    [workflow, appNameMap],
  );

  const nodeToAppGroup = useMemo(() => {
    const map = new Map<string, string>();
    for (const [groupId, memberIds] of appGroups) {
      for (const nodeId of memberIds) {
        map.set(nodeId, groupId);
      }
    }
    return map;
  }, [appGroups]);

  const appGroupMeta = useMemo(() => {
    const meta = new Map<string, AppGroupMeta>();
    for (const [groupId, memberIds] of appGroups) {
      if (memberIds.length === 0) continue;
      const anchorId = groupId.replace("appgroup-", "");
      const appName = appNameMap.get(anchorId);
      if (appName == null) continue;
      meta.set(groupId, {
        appName,
        color: GROUP_COLORS[hashAppName(appName)],
        anchorId,
      });
    }
    return meta;
  }, [appGroups, appNameMap]);

  const toggleAppCollapse = useCallback((groupId: string) => {
    setCollapsedApps((prev) => {
      const next = new Set(prev);
      if (next.has(groupId)) next.delete(groupId);
      else next.add(groupId);
      return next;
    });
  }, []);

  // Default new groups to collapsed; clean removed groups
  const knownGroupsRef = useRef<Set<string>>(new Set());
  useEffect(() => {
    const currentGroupIds = new Set(appGroups.keys());
    const newGroups: string[] = [];
    for (const groupId of currentGroupIds) {
      if (!knownGroupsRef.current.has(groupId)) newGroups.push(groupId);
    }
    setCollapsedApps((prev) => {
      const next = new Set(prev);
      for (const groupId of newGroups) next.add(groupId);
      for (const id of next) {
        if (!currentGroupIds.has(id)) next.delete(id);
      }
      return next;
    });
    knownGroupsRef.current = currentGroupIds;
  }, [appGroups]);

  const hiddenNodeIds = useMemo(() => {
    const ids = new Set<string>();
    for (const [groupId, memberIds] of appGroups) {
      if (collapsedApps.has(groupId)) {
        for (const nodeId of memberIds) {
          ids.add(nodeId);
        }
      }
    }
    return ids;
  }, [appGroups, collapsedApps]);

  const collapsedAppEdgeRewrites = useMemo(() => {
    const map = new Map<string, string>();
    for (const [groupId, memberIds] of appGroups) {
      if (!collapsedApps.has(groupId)) continue;
      const meta = appGroupMeta.get(groupId);
      if (!meta) continue;
      for (const nodeId of memberIds) {
        map.set(nodeId, meta.anchorId);
      }
    }
    return map;
  }, [appGroups, collapsedApps, appGroupMeta]);

  return {
    collapsedApps,
    appGroups,
    nodeToAppGroup,
    appGroupMeta,
    toggleAppCollapse,
    hiddenNodeIds,
    collapsedAppEdgeRewrites,
  };
}
