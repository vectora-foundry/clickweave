import { useEffect, useLayoutEffect, useRef, useState } from "react";
import type {
  AmbiguityCandidateView,
  AmbiguityResolution,
} from "../store/slices/agentSlice";
import { Modal } from "./Modal";

interface Props {
  resolution: AmbiguityResolution;
  onClose: () => void;
}

const NON_CHOSEN_STROKE = "#94a3b8"; // slate-400
const CHOSEN_STROKE = "#10b981"; // emerald-500
const CHOSEN_FILL = "rgba(16, 185, 129, 0.15)";

/**
 * Modal showing the screenshot taken at agent-decision time with every
 * candidate's rect overlaid. The chosen candidate gets a thicker, accented
 * outline plus a subtle fill tint so it's immediately obvious which one the
 * agent committed to.
 *
 * Rects come from the CDP viewport (CSS pixels). The captured image may be at
 * a different natural resolution than the viewport (device-pixel ratio), so
 * we scale by the ratio between the rendered image size and the natural image
 * size.
 */
export function AmbiguityResolutionModal({ resolution, onClose }: Props) {
  const imageRef = useRef<HTMLImageElement>(null);
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const dialogRef = useRef<HTMLDivElement>(null);
  const [imageLoaded, setImageLoaded] = useState(false);

  useEffect(() => {
    // Focus the dialog on mount so Esc + tab trapping work.
    dialogRef.current?.focus();
  }, []);

  useLayoutEffect(() => {
    if (!imageLoaded) return;
    const img = imageRef.current;
    const canvas = canvasRef.current;
    if (!img || !canvas) return;

    const draw = () => {
      const rect = img.getBoundingClientRect();
      const dpr = window.devicePixelRatio || 1;
      canvas.width = Math.max(1, Math.floor(rect.width * dpr));
      canvas.height = Math.max(1, Math.floor(rect.height * dpr));
      canvas.style.width = `${rect.width}px`;
      canvas.style.height = `${rect.height}px`;

      const ctx = canvas.getContext("2d");
      if (!ctx) return;
      ctx.setTransform(1, 0, 0, 1, 0, 0);
      ctx.scale(dpr, dpr);
      ctx.clearRect(0, 0, rect.width, rect.height);

      // Scale CDP-viewport coordinates (naturalWidth/Height) to the rendered
      // image size. If naturalWidth is 0 (still decoding), skip.
      if (img.naturalWidth === 0 || img.naturalHeight === 0) return;
      const sx = rect.width / img.naturalWidth;
      const sy = rect.height / img.naturalHeight;

      const drawRect = (c: AmbiguityCandidateView, isChosen: boolean) => {
        if (!c.rect) return;
        const x = c.rect.x * sx;
        const y = c.rect.y * sy;
        const w = c.rect.width * sx;
        const h = c.rect.height * sy;

        if (isChosen) {
          ctx.fillStyle = CHOSEN_FILL;
          ctx.fillRect(x, y, w, h);
          ctx.strokeStyle = CHOSEN_STROKE;
          ctx.lineWidth = 3;
        } else {
          ctx.strokeStyle = NON_CHOSEN_STROKE;
          ctx.lineWidth = 2;
        }
        ctx.strokeRect(x, y, w, h);

        // Label with uid (small text on a dark background for readability).
        const label = `uid=${c.uid}`;
        ctx.font = "11px ui-sans-serif, system-ui, sans-serif";
        const metrics = ctx.measureText(label);
        const padding = 4;
        const labelWidth = metrics.width + padding * 2;
        const labelHeight = 16;
        ctx.fillStyle = isChosen
          ? CHOSEN_STROKE
          : "rgba(15, 23, 42, 0.85)";
        ctx.fillRect(x, Math.max(0, y - labelHeight), labelWidth, labelHeight);
        ctx.fillStyle = "#ffffff";
        ctx.fillText(label, x + padding, Math.max(11, y - 4));
      };

      // Draw non-chosen first so the chosen accentuation sits on top.
      for (const c of resolution.candidates) {
        if (c.uid !== resolution.chosenUid) drawRect(c, false);
      }
      for (const c of resolution.candidates) {
        if (c.uid === resolution.chosenUid) drawRect(c, true);
      }
    };

    draw();
    const ro = new ResizeObserver(() => draw());
    ro.observe(img);
    window.addEventListener("resize", draw);
    return () => {
      ro.disconnect();
      window.removeEventListener("resize", draw);
    };
  }, [imageLoaded, resolution]);

  return (
    <Modal open onClose={onClose} className="w-[min(960px,95vw)] max-h-[92vh]">
      <div
        role="dialog"
        aria-modal="true"
        aria-labelledby="ambiguity-modal-title"
        ref={dialogRef}
        tabIndex={-1}
        className="flex max-h-[92vh] flex-col overflow-hidden rounded-lg border border-[var(--border)] bg-[var(--bg-panel)] shadow-2xl outline-none"
      >
        <div className="flex items-start justify-between gap-3 border-b border-[var(--border)] px-5 py-3">
          <div>
            <h3
              id="ambiguity-modal-title"
              className="text-sm font-medium text-[var(--text-primary)]"
            >
              Resolved ambiguity on &ldquo;{resolution.target}&rdquo;
            </h3>
            <p className="mt-0.5 text-[11px] text-[var(--text-muted)]">
              {resolution.candidates.length} candidates matched — agent picked{" "}
              <span className="font-mono text-emerald-400">
                uid={resolution.chosenUid}
              </span>
            </p>
          </div>
          <button
            onClick={onClose}
            aria-label="Close"
            className="rounded px-2 py-1 text-sm text-[var(--text-muted)] hover:bg-[var(--bg-hover)] hover:text-[var(--text-primary)]"
          >
            &times;
          </button>
        </div>

        <div className="flex-1 overflow-y-auto px-5 py-4">
          <section className="mb-4">
            <h4 className="mb-1 text-[11px] font-medium uppercase tracking-wide text-[var(--text-muted)]">
              Reasoning
            </h4>
            <p className="rounded bg-[var(--bg-dark)] px-3 py-2 text-xs leading-relaxed text-[var(--text-secondary)]">
              {resolution.reasoning}
            </p>
          </section>

          <section className="mb-4">
            <h4 className="mb-1 text-[11px] font-medium uppercase tracking-wide text-[var(--text-muted)]">
              Screenshot at decision time
            </h4>
            <div className="relative inline-block max-w-full rounded border border-[var(--border)] bg-black">
              <img
                ref={imageRef}
                src={`data:image/png;base64,${resolution.screenshotBase64}`}
                alt="Screenshot with candidate overlays"
                className="block max-h-[60vh] max-w-full"
                onLoad={() => setImageLoaded(true)}
              />
              <canvas
                ref={canvasRef}
                className="pointer-events-none absolute left-0 top-0"
                aria-hidden="true"
              />
            </div>
          </section>

          <section>
            <h4 className="mb-1 text-[11px] font-medium uppercase tracking-wide text-[var(--text-muted)]">
              Candidates
            </h4>
            <ul className="space-y-1 text-xs">
              {resolution.candidates.map((c) => {
                const isChosen = c.uid === resolution.chosenUid;
                return (
                  <li
                    key={c.uid}
                    className={`flex items-start gap-2 rounded px-2 py-1.5 ${
                      isChosen
                        ? "bg-emerald-500/10 text-[var(--text-primary)]"
                        : "bg-[var(--bg-dark)] text-[var(--text-secondary)]"
                    }`}
                  >
                    <span
                      className={`mt-0.5 inline-block h-3 w-3 rounded-sm ${
                        isChosen
                          ? "bg-emerald-500"
                          : "border border-slate-400 bg-transparent"
                      }`}
                      aria-hidden="true"
                    />
                    <div className="min-w-0 flex-1">
                      <div className="font-mono">
                        uid={c.uid}
                        {isChosen && (
                          <span className="ml-2 rounded bg-emerald-500/20 px-1.5 text-[10px] font-medium text-emerald-300">
                            picked
                          </span>
                        )}
                      </div>
                      <div className="truncate text-[11px] text-[var(--text-muted)]">
                        {c.snippet}
                      </div>
                    </div>
                  </li>
                );
              })}
            </ul>
          </section>
        </div>
      </div>
    </Modal>
  );
}
