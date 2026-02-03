import { useState, useCallback, type ReactNode } from 'react';
import { motion, AnimatePresence } from 'framer-motion';
import { PanelLeftClose, PanelLeftOpen, PanelRightClose, PanelRightOpen } from 'lucide-react';

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
  const [isSidebarOpen, setIsSidebarOpen] = useState(true);
  const [isRightPanelOpen, setIsRightPanelOpen] = useState(true);

  const toggleSidebar = useCallback(() => setIsSidebarOpen(prev => !prev), []);
  const toggleRightPanel = useCallback(() => setIsRightPanelOpen(prev => !prev), []);

  return (
    <div className="h-screen flex flex-col bg-[#080808]">
      {/* Top bar with toggle controls */}
      <header className="h-12 flex-shrink-0 border-b border-[#1c1c1c] flex items-center justify-between px-4 bg-[#0d0d0d]">
        <div className="flex items-center gap-3">
          <button
            onClick={toggleSidebar}
            className="p-1.5 rounded hover:bg-[#1c1c1c] text-[#888888] hover:text-[#ededed] transition-colors"
            aria-label={isSidebarOpen ? 'Close sidebar' : 'Open sidebar'}
          >
            {isSidebarOpen ? <PanelLeftClose size={18} /> : <PanelLeftOpen size={18} />}
          </button>
          <span className="text-sm font-medium text-[#ededed]">CVC Reviewer</span>
        </div>

        <button
          onClick={toggleRightPanel}
          className="p-1.5 rounded hover:bg-[#1c1c1c] text-[#888888] hover:text-[#ededed] transition-colors"
          aria-label={isRightPanelOpen ? 'Close timeline' : 'Open timeline'}
        >
          {isRightPanelOpen ? <PanelRightClose size={18} /> : <PanelRightOpen size={18} />}
        </button>
      </header>

      {/* Main content area */}
      <div className="flex-1 flex overflow-hidden">
        {/* Sidebar */}
        <AnimatePresence initial={false}>
          {isSidebarOpen && (
            <motion.aside
              initial={{ width: 0, opacity: 0 }}
              animate={{ width: sidebarWidth, opacity: 1 }}
              exit={{ width: 0, opacity: 0 }}
              transition={{ duration: 0.15, ease: 'easeInOut' }}
              className="flex-shrink-0 border-r border-[#1c1c1c] bg-[#0d0d0d] overflow-hidden"
            >
              <div className="h-full overflow-y-auto" style={{ width: sidebarWidth }}>
                {sidebar}
              </div>
            </motion.aside>
          )}
        </AnimatePresence>

        {/* Main content (flex grow) */}
        <main className="flex-1 overflow-auto bg-[#080808]">
          {main}
        </main>

        {/* Right panel (Shadow Timeline) */}
        <AnimatePresence initial={false}>
          {isRightPanelOpen && (
            <motion.aside
              initial={{ width: 0, opacity: 0 }}
              animate={{ width: rightPanelWidth, opacity: 1 }}
              exit={{ width: 0, opacity: 0 }}
              transition={{ duration: 0.15, ease: 'easeInOut' }}
              className="flex-shrink-0 border-l border-[#1c1c1c] bg-[#0d0d0d] overflow-hidden"
            >
              <div className="h-full overflow-y-auto" style={{ width: rightPanelWidth }}>
                {rightPanel}
              </div>
            </motion.aside>
          )}
        </AnimatePresence>
      </div>
    </div>
  );
}
