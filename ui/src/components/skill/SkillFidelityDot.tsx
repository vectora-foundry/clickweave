/**
 * Phase 1 placeholder for the per-section fidelity dot (D7).
 *
 * Fidelity is coarse-grained replay confidence stamped by the replay engine
 * (Phase 2+). In Phase 1 every section renders in the `no_data` state —
 * a neutral grey dot — until the replay engine begins providing signals.
 */

export type FidelityLevel = "solid" | "repaired" | "brittle" | "no_data";

interface SkillFidelityDotProps {
  fidelity?: FidelityLevel;
  /** Optional ARIA label for accessibility. Defaults to the fidelity level. */
  label?: string;
}

const DOT_COLORS: Record<FidelityLevel, string> = {
  solid: "bg-emerald-500",
  repaired: "bg-amber-400",
  brittle: "bg-red-400",
  no_data: "bg-[var(--text-muted)] opacity-30",
};

export function SkillFidelityDot({
  fidelity = "no_data",
  label,
}: SkillFidelityDotProps) {
  const ariaLabel = label ?? fidelity.replace("_", " ");
  return (
    <span
      role="img"
      aria-label={ariaLabel}
      className={`inline-block h-2 w-2 shrink-0 rounded-full ${DOT_COLORS[fidelity]}`}
    />
  );
}
