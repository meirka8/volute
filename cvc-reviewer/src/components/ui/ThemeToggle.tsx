import { MoonStar, SunMedium } from "lucide-react";
import { clsx } from "clsx";
import { toggleReviewerTheme } from "../../lib/theme";

export function ThemeToggle({
  compact = false,
  className,
}: {
  compact?: boolean;
  className?: string;
}) {
  return (
    <button
      type="button"
      onClick={() => {
        toggleReviewerTheme();
      }}
      aria-label="Toggle color theme"
      title="Toggle color theme"
      className={clsx(
        "rr-panel inline-flex items-center gap-2 rounded-full text-sm font-medium text-ink transition-colors hover:bg-surface hover:text-ink",
        compact ? "h-10 w-10 justify-center px-0 py-0" : "justify-between px-3 py-2.5",
        className,
      )}
    >
      <span className="flex items-center gap-2">
        <SunMedium className="theme-light-only h-4 w-4 text-action" />
        <MoonStar className="theme-dark-only h-4 w-4 text-action" />
        {!compact && <span>Theme</span>}
      </span>

      {!compact && (
        <span className="rounded-full border border-line bg-canvas/70 px-2 py-1 text-[11px] uppercase tracking-[0.18em] text-muted">
          <span className="theme-light-only">Light</span>
          <span className="theme-dark-only">Dark</span>
        </span>
      )}
    </button>
  );
}
