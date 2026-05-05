import { LayoutDashboard, Network, PanelLeftClose, PanelLeftOpen } from "lucide-react";
import { useStore } from "../../store/useAppStore";

const NAV_ITEMS = [
  { id: "overview" as const, label: "Overview", Icon: LayoutDashboard },
  { id: "canvas" as const, label: "Trace", Icon: Network },
];

export function Sidebar() {
  const currentView = useStore((s) => s.currentView);
  const setCurrentView = useStore((s) => s.setCurrentView);
  const collapsed = useStore((s) => s.utilitySidebarCollapsed);
  const toggle = useStore((s) => s.toggleUtilitySidebar);

  const width = collapsed ? "w-[44px]" : "w-[200px]";

  return (
    <nav
      className={`flex h-full ${width} shrink-0 flex-col border-r border-[var(--hairline)] bg-[var(--oxide)] transition-[width] duration-150`}
    >
      <div
        className={`flex items-center pt-4 ${collapsed ? "justify-center px-0" : "justify-between px-4"}`}
      >
        {!collapsed && (
          <span className="text-[10px] font-medium tracking-[0.18em] text-[var(--text-muted)]">
            UTILITY
          </span>
        )}
        <button
          onClick={toggle}
          aria-label={collapsed ? "Expand utility sidebar" : "Collapse utility sidebar"}
          aria-expanded={!collapsed}
          title={collapsed ? "Expand" : "Collapse"}
          className="flex h-6 w-6 items-center justify-center rounded text-[var(--text-muted)] hover:bg-[var(--bg-hover)] hover:text-[var(--text-primary)]"
        >
          {collapsed ? (
            <PanelLeftOpen size={13} strokeWidth={1.5} />
          ) : (
            <PanelLeftClose size={13} strokeWidth={1.5} />
          )}
        </button>
      </div>
      <ul className="mt-2 flex-1 px-2">
        {NAV_ITEMS.map((item) => {
          const active = item.id === currentView;
          const { Icon } = item;
          return (
            <li key={item.id}>
              <button
                onClick={() => setCurrentView(item.id)}
                title={collapsed ? item.label : undefined}
                aria-label={collapsed ? item.label : undefined}
                className={`relative flex w-full items-center rounded-md py-1.5 text-[12px] ${
                  collapsed ? "justify-center px-0" : "px-3"
                } ${
                  active
                    ? "bg-[var(--bloom-coral)] text-[var(--text-primary)]"
                    : "text-[var(--text-secondary)] hover:bg-[var(--bg-hover)] hover:text-[var(--text-primary)]"
                }`}
                aria-current={active ? "page" : undefined}
              >
                {active && (
                  <span className="cw-sidebar-active-draw absolute left-0 top-1.5 h-[calc(100%-12px)] w-0.5 rounded-full bg-[var(--accent-coral)]" />
                )}
                {collapsed ? (
                  <Icon size={14} strokeWidth={1.5} />
                ) : (
                  <span className={active ? "ml-2" : ""}>{item.label}</span>
                )}
              </button>
            </li>
          );
        })}
      </ul>
    </nav>
  );
}
