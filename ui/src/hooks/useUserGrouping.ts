import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { Workflow } from "../bindings";

export interface UserGroupMeta {
  name: string;
  color: string;
  anchorId: string;       // first node in node_ids (topological order)
  flatMemberCount: number; // total nodes including subgroup members
  parentGroupId: string | null;
}

export function useUserGrouping(workflow: Workflow) {
  const [collapsedUserGroups, setCollapsedUserGroups] = useState<Set<string>>(new Set());

  // Stabilise the groups reference: `?? []` would create a new array ref every render
  // if workflow.groups is undefined (makeWorkflow always sets it, but guard for safety).
  const groups = useMemo(() => workflow.groups ?? [], [workflow.groups]);

  // Map from node ID → group ID
  const nodeToUserGroup = useMemo(() => {
    const map = new Map<string, string>();
    for (const g of groups) {
      for (const nodeId of g.node_ids) {
        map.set(nodeId, g.id);
      }
    }
    return map;
  }, [groups]);

  // Map from group ID → metadata
  const userGroupMeta = useMemo(() => {
    const meta = new Map<string, UserGroupMeta>();
    for (const g of groups) {
      if (g.node_ids.length === 0) continue;
      meta.set(g.id, {
        name: g.name,
        color: g.color,
        anchorId: g.node_ids[0],
        flatMemberCount: g.node_ids.length,
        parentGroupId: g.parent_group_id,
      });
    }
    return meta;
  }, [groups]);

  const toggleUserGroupCollapse = useCallback((groupId: string) => {
    setCollapsedUserGroups((prev) => {
      const next = new Set(prev);
      if (next.has(groupId)) next.delete(groupId);
      else next.add(groupId);
      return next;
    });
  }, []);

  // New groups default to expanded — only clean up stale IDs when groups are removed.
  const knownGroupsRef = useRef<Set<string>>(new Set());
  useEffect(() => {
    const currentGroupIds = new Set(groups.map((g) => g.id));
    let hasStale = false;
    for (const id of knownGroupsRef.current) {
      if (!currentGroupIds.has(id)) {
        hasStale = true;
        break;
      }
    }
    // No new groups to auto-collapse (expanded by default), only clean stale ones.
    if (!hasStale) {
      knownGroupsRef.current = currentGroupIds;
      return;
    }
    setCollapsedUserGroups((prev) => {
      const next = new Set(prev);
      for (const id of next) {
        if (!currentGroupIds.has(id)) next.delete(id);
      }
      return next;
    });
    knownGroupsRef.current = currentGroupIds;
  }, [groups]);

  // Edge rewrites for collapsed groups.
  // For each collapsed group, map every member node ID to the anchor.
  // Skip subgroups whose parent is also collapsed (parent takes precedence).
  const userGroupEdgeRewrites = useMemo(() => {
    const map = new Map<string, string>();
    for (const g of groups) {
      if (!collapsedUserGroups.has(g.id)) continue;
      // Skip if parent is also collapsed — parent's rewrites take precedence
      if (g.parent_group_id !== null && collapsedUserGroups.has(g.parent_group_id)) continue;
      const anchorId = g.node_ids[0];
      for (const nodeId of g.node_ids) {
        map.set(nodeId, anchorId);
      }
    }
    return map;
  }, [groups, collapsedUserGroups]);

  // Hidden nodes: non-anchor members of collapsed groups.
  // When a parent group is collapsed, hide ALL its members except the parent anchor.
  // Never hide the parent's anchor even if it also appears in a subgroup.
  const hiddenUserGroupNodeIds = useMemo(() => {
    const hidden = new Set<string>();
    for (const g of groups) {
      if (!collapsedUserGroups.has(g.id)) continue;
      const anchorId = g.node_ids[0];
      for (const nodeId of g.node_ids) {
        if (nodeId !== anchorId) {
          hidden.add(nodeId);
        }
      }
    }
    // Remove parent anchors that may have been added by a child group iteration:
    // a node that is a parent group's anchor must never be hidden, regardless of subgroups.
    for (const g of groups) {
      if (!collapsedUserGroups.has(g.id)) continue;
      // This group's anchor must not be hidden by any other group's logic
      hidden.delete(g.node_ids[0]);
    }
    return hidden;
  }, [groups, collapsedUserGroups]);

  return {
    collapsedUserGroups,
    nodeToUserGroup,
    userGroupMeta,
    toggleUserGroupCollapse,
    userGroupEdgeRewrites,
    hiddenUserGroupNodeIds,
  };
}
