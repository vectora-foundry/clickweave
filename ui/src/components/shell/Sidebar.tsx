import { useStore } from "../../store/useAppStore";

const NAV_ITEMS = [
  { id: "overview" as const, label: "Overview" },
  { id: "canvas" as const, label: "Canvas" },
];

export function Sidebar() {
  const currentView = useStore((s) => s.currentView);
  const setCurrentView = useStore((s) => s.setCurrentView);

  return (
    <nav className="flex h-full w-[200px] shrink-0 flex-col border-r border-[var(--hairline)] bg-[var(--oxide)]">
      <div className="px-4 pt-4 text-[10px] font-medium tracking-[0.18em] text-[var(--text-muted)]">
        UTILITY
      </div>
      <ul className="mt-2 flex-1 px-2">
        {NAV_ITEMS.map((item) => {
          const active = item.id === currentView;
          return (
            <li key={item.id}>
              <button
                onClick={() => setCurrentView(item.id)}
                className={`relative flex w-full items-center rounded-md px-3 py-1.5 text-[12px] ${
                  active
                    ? "bg-[var(--bloom-coral)] text-[var(--text-primary)]"
                    : "text-[var(--text-secondary)] hover:bg-[var(--bg-hover)] hover:text-[var(--text-primary)]"
                }`}
                aria-current={active ? "page" : undefined}
              >
                {active && (
                  <span className="absolute left-0 top-1.5 h-[calc(100%-12px)] w-0.5 rounded-full bg-[var(--accent-coral)]" />
                )}
                <span className={active ? "ml-2" : ""}>{item.label}</span>
              </button>
            </li>
          );
        })}
      </ul>
    </nav>
  );
}
