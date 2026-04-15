import type { AmbiguityResolution } from "../store/slices/agentSlice";

interface Props {
  resolution: AmbiguityResolution;
  onOpen: () => void;
}

const REASONING_PREVIEW_CHARS = 60;

/**
 * Persistent agent-panel card rendered when the engine resolves an ambiguous
 * CDP target. Clicking opens the full modal with the screenshot and every
 * candidate's bounding rect overlaid.
 */
export function AmbiguityResolutionCard({ resolution, onOpen }: Props) {
  const preview =
    resolution.reasoning.length > REASONING_PREVIEW_CHARS
      ? `${resolution.reasoning.slice(0, REASONING_PREVIEW_CHARS)}\u2026`
      : resolution.reasoning;

  return (
    <button
      type="button"
      onClick={onOpen}
      data-testid="ambiguity-resolution-card"
      className="group mx-3 mb-2 block w-[calc(100%-1.5rem)] rounded-lg border border-emerald-500/40 bg-emerald-500/10 px-3 py-2.5 text-left transition-colors hover:border-emerald-400/60 hover:bg-emerald-500/15 focus:outline-none focus:ring-2 focus:ring-emerald-400/40"
      aria-label={`Open ambiguity resolution for ${resolution.target}`}
    >
      <div className="flex items-center gap-1.5">
        <span
          aria-hidden="true"
          className="inline-block h-2 w-2 rounded-full bg-emerald-400"
        />
        <span className="text-[11px] font-medium uppercase tracking-wide text-emerald-300">
          Ambiguity resolved
        </span>
        <span className="ml-auto text-[10px] text-[var(--text-muted)] group-hover:text-[var(--text-secondary)]">
          open &rsaquo;
        </span>
      </div>
      <p className="mt-1 text-xs text-[var(--text-primary)]">
        Resolved &ldquo;
        <span className="font-medium">{resolution.target}</span>
        &rdquo; &mdash; picked uid=
        <span className="font-mono text-emerald-300">
          {resolution.chosenUid}
        </span>
      </p>
      {preview && (
        <p className="mt-0.5 line-clamp-2 text-[11px] leading-relaxed text-[var(--text-muted)]">
          {preview}
        </p>
      )}
    </button>
  );
}
