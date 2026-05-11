import { useState, useCallback, useEffect, type ReactNode } from 'react';
import { motion, AnimatePresence } from 'framer-motion';
import { PanelLeftClose, PanelLeftOpen, PanelRightClose, PanelRightOpen } from 'lucide-react';
import { ThemeToggle } from './ThemeToggle';

interface ThreePaneLayoutProps {
  sidebar: ReactNode;
  main: ReactNode;
  rightPanel: ReactNode;
  sidebarWidth?: number;
  rightPanelWidth?: number;
}

export function ThreePaneLayout({
  sidebar,
  main,
  rightPanel,
  sidebarWidth = 260,
  rightPanelWidth = 400,
}: ThreePaneLayoutProps) {
  const [isMobile, setIsMobile] = useState(
    typeof window !== 'undefined' ? window.innerWidth < 1024 : false,
  );
  const [isSidebarOpen, setIsSidebarOpen] = useState(
    typeof window !== 'undefined' ? window.innerWidth >= 1024 : true,
  );
  const [isRightPanelOpen, setIsRightPanelOpen] = useState(
    typeof window !== 'undefined' ? window.innerWidth >= 1024 : true,
  );

  useEffect(() => {
    const mediaQuery = window.matchMedia('(max-width: 1023px)');
    const handleChange = (event: MediaQueryListEvent) => {
      setIsMobile(event.matches);
      setIsSidebarOpen(!event.matches);
      setIsRightPanelOpen(!event.matches);
    };

    mediaQuery.addEventListener('change', handleChange);

    return () => mediaQuery.removeEventListener('change', handleChange);
  }, []);

  const toggleSidebar = useCallback(() => setIsSidebarOpen(prev => !prev), []);
  const toggleRightPanel = useCallback(() => setIsRightPanelOpen(prev => !prev), []);
  const closePanels = useCallback(() => {
    setIsSidebarOpen(false);
    setIsRightPanelOpen(false);
  }, []);

  return (
    <div className="flex h-screen flex-col bg-transparent text-ink">
      <header className="rr-panel mx-3 mb-3 mt-3 flex h-14 flex-shrink-0 items-center justify-between rounded-[1.5rem] px-4 sm:mx-4">
        <div className="flex items-center gap-3">
          <button
            onClick={toggleSidebar}
            className="rounded-full p-2 text-muted transition-colors hover:bg-surface hover:text-ink"
            aria-label={isSidebarOpen ? 'Close sidebar' : 'Open sidebar'}
          >
            {isSidebarOpen ? <PanelLeftClose size={18} /> : <PanelLeftOpen size={18} />}
          </button>
          <div>
            <div className="text-[10px] font-semibold uppercase tracking-[0.24em] text-action">Review Surface</div>
            <span className="font-serif text-sm font-medium text-ink">CVC Reviewer</span>
          </div>
        </div>

        <div className="flex items-center gap-2">
          <ThemeToggle compact />
          <button
            onClick={toggleRightPanel}
            className="rounded-full p-2 text-muted transition-colors hover:bg-surface hover:text-ink"
            aria-label={isRightPanelOpen ? 'Close timeline' : 'Open timeline'}
          >
            {isRightPanelOpen ? <PanelRightClose size={18} /> : <PanelRightOpen size={18} />}
          </button>
        </div>
      </header>

      <div className="flex flex-1 overflow-hidden px-3 pb-3 sm:px-4 sm:pb-4">
        {isMobile ? (
          <>
            <main className="rr-panel flex-1 overflow-auto rounded-[1.5rem]">{main}</main>

            <AnimatePresence>
              {(isSidebarOpen || isRightPanelOpen) && (
                <motion.button
                  type="button"
                  aria-label="Close panel overlay"
                  initial={{ opacity: 0 }}
                  animate={{ opacity: 1 }}
                  exit={{ opacity: 0 }}
                  className="rr-overlay fixed inset-0 z-30 backdrop-blur-sm"
                  onClick={closePanels}
                />
              )}
            </AnimatePresence>

            <AnimatePresence initial={false}>
              {isSidebarOpen && (
                <motion.aside
                  initial={{ x: -24, opacity: 0 }}
                  animate={{ x: 0, opacity: 1 }}
                  exit={{ x: -24, opacity: 0 }}
                  transition={{ duration: 0.18, ease: 'easeInOut' }}
                  className="rr-panel fixed inset-y-20 left-3 z-40 overflow-hidden rounded-[1.5rem] sm:left-4"
                  style={{ width: Math.min(sidebarWidth, 320) }}
                >
                  <div className="h-full overflow-y-auto">{sidebar}</div>
                </motion.aside>
              )}
            </AnimatePresence>

            <AnimatePresence initial={false}>
              {isRightPanelOpen && (
                <motion.aside
                  initial={{ x: 24, opacity: 0 }}
                  animate={{ x: 0, opacity: 1 }}
                  exit={{ x: 24, opacity: 0 }}
                  transition={{ duration: 0.18, ease: 'easeInOut' }}
                  className="rr-panel fixed inset-y-20 right-3 z-40 overflow-hidden rounded-[1.5rem] sm:right-4"
                  style={{ width: Math.min(rightPanelWidth, 360) }}
                >
                  <div className="h-full overflow-y-auto">{rightPanel}</div>
                </motion.aside>
              )}
            </AnimatePresence>
          </>
        ) : (
          <>
            <AnimatePresence initial={false}>
              {isSidebarOpen && (
                <motion.aside
                  initial={{ width: 0, opacity: 0 }}
                  animate={{ width: sidebarWidth, opacity: 1 }}
                  exit={{ width: 0, opacity: 0 }}
                  transition={{ duration: 0.15, ease: 'easeInOut' }}
                  className="rr-panel flex-shrink-0 overflow-hidden rounded-[1.5rem]"
                >
                  <div className="h-full overflow-y-auto" style={{ width: sidebarWidth }}>
                    {sidebar}
                  </div>
                </motion.aside>
              )}
            </AnimatePresence>

            <main className="mx-3 flex-1 overflow-auto rounded-[1.5rem] bg-transparent">{main}</main>

            <AnimatePresence initial={false}>
              {isRightPanelOpen && (
                <motion.aside
                  initial={{ width: 0, opacity: 0 }}
                  animate={{ width: rightPanelWidth, opacity: 1 }}
                  exit={{ width: 0, opacity: 0 }}
                  transition={{ duration: 0.15, ease: 'easeInOut' }}
                  className="rr-panel flex-shrink-0 overflow-hidden rounded-[1.5rem]"
                >
                  <div className="h-full overflow-y-auto" style={{ width: rightPanelWidth }}>
                    {rightPanel}
                  </div>
                </motion.aside>
              )}
            </AnimatePresence>
          </>
        )}
      </div>
    </div>
  );
}
