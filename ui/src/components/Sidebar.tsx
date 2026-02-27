interface SidebarProps {
  collapsed: boolean;
  onToggle: () => void;
}

const navItems = [
  { icon: "H", label: "Home" },
  { icon: "T", label: "Templates" },
  { icon: "V", label: "Variables" },
  { icon: "E", label: "Executions" },
  { icon: "?", label: "Help" },
];

export function Sidebar({ collapsed, onToggle }: SidebarProps) {
  return (
    <div
      className={`flex flex-col border-r border-[var(--border)] bg-[var(--bg-panel)] transition-all duration-200 ${
        collapsed ? "w-12" : "w-48"
      }`}
    >
      {/* Toggle */}
      <button
        onClick={onToggle}
        className="flex h-10 items-center justify-center border-b border-[var(--border)] text-[var(--text-muted)] hover:bg-[var(--bg-hover)] hover:text-[var(--text-secondary)]"
        title={collapsed ? "Expand sidebar" : "Collapse sidebar"}
      >
        <svg
          width="14"
          height="14"
          viewBox="0 0 16 16"
          fill="none"
          stroke="currentColor"
          strokeWidth="1.5"
          strokeLinecap="round"
        >
          <path d="M2 4h12M2 8h12M2 12h12" />
        </svg>
      </button>

      {/* Nav items */}
      <nav className="flex flex-1 flex-col gap-0.5 p-1.5">
        {navItems.map((item) => (
          <button
            key={item.label}
            className="flex items-center gap-2 rounded px-2.5 py-2 text-sm text-[var(--text-secondary)] hover:bg-[var(--bg-hover)] hover:text-[var(--text-primary)]"
          >
            <span className="inline-flex h-5 w-5 items-center justify-center text-xs font-bold">
              {item.icon}
            </span>
            {!collapsed && <span>{item.label}</span>}
          </button>
        ))}
      </nav>
    </div>
  );
}
