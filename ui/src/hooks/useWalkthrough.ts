import { useStore } from "../store/useAppStore";
import { useShallow } from "zustand/react/shallow";

/**
 * Selector hook that extracts all walkthrough state and actions from the store.
 * Uses `useShallow` to batch the selection and avoid unnecessary re-renders.
 *
 * Intended as the single access point for WalkthroughPanel and other
 * walkthrough-aware components.
 */
export function useWalkthrough() {
  return useStore(
    useShallow((s) => ({
      // State
      status: s.walkthroughStatus,
      panelOpen: s.walkthroughPanelOpen,
      error: s.walkthroughError,
      events: s.walkthroughEvents,
      actions: s.walkthroughActions,
      draft: s.walkthroughDraft,
      warnings: s.walkthroughWarnings,
      annotations: s.walkthroughAnnotations,
      expandedAction: s.walkthroughExpandedAction,
      actionNodeMap: s.walkthroughActionNodeMap,
      cdpModalOpen: s.walkthroughCdpModalOpen,
      cdpProgress: s.walkthroughCdpProgress,
      nodeOrder: s.walkthroughNodeOrder,

      // Core actions
      setStatus: s.setWalkthroughStatus,
      setPanelOpen: s.setWalkthroughPanelOpen,
      setDraft: s.setWalkthroughDraft,

      // Recording actions
      pushEvent: s.pushWalkthroughEvent,
      pushCdpProgress: s.pushCdpProgress,
      openCdpModal: s.openCdpModal,
      closeCdpModal: s.closeCdpModal,
      startWalkthrough: s.startWalkthrough,
      pauseWalkthrough: s.pauseWalkthrough,
      resumeWalkthrough: s.resumeWalkthrough,
      stopWalkthrough: s.stopWalkthrough,
      cancelWalkthrough: s.cancelWalkthrough,

      // Review actions
      setExpandedAction: s.setWalkthroughExpandedAction,
      keepCandidate: s.keepCandidate,
      dismissCandidate: s.dismissCandidate,
      deleteNode: s.deleteNode,
      restoreNode: s.restoreNode,
      renameNode: s.renameNode,
      overrideTarget: s.overrideTarget,
      promoteToVariable: s.promoteToVariable,
      removeVariablePromotion: s.removeVariablePromotion,
      resetAnnotations: s.resetAnnotations,
      reorderNode: s.reorderNode,
      reorderGroup: s.reorderGroup,
      applyDraftToCanvas: s.applyDraftToCanvas,
      discardDraft: s.discardDraft,
    })),
  );
}
