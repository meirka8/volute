import { useEffect, useState, useCallback } from "react";
import { Command } from "cmdk";
import { motion, AnimatePresence } from "framer-motion";
import { Search, File, Check, Keyboard } from "lucide-react";
import { clsx } from "clsx";

export interface CommandAction {
  id: string;
  label: string;
  shortcut?: string;
  icon?: React.ReactNode;
  onSelect: () => void;
  group?: string;
}

interface CommandPaletteProps {
  isOpen: boolean;
  onClose: () => void;
  actions: CommandAction[];
  files?: { filename: string; isViewed: boolean }[];
  onSelectFile?: (filename: string) => void;
}

export function CommandPalette({
  isOpen,
  onClose,
  actions,
  files = [],
  onSelectFile,
}: CommandPaletteProps) {
  const [search, setSearch] = useState("");

  // Close on escape
  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        onClose();
      }
    };

    if (isOpen) {
      document.addEventListener("keydown", handleKeyDown);
      return () => document.removeEventListener("keydown", handleKeyDown);
    }
  }, [isOpen, onClose]);

  // Reset search when closing
  useEffect(() => {
    if (!isOpen) {
      setSearch("");
    }
  }, [isOpen]);

  // Group actions by their group property
  const groupedActions = actions.reduce(
    (acc, action) => {
      const group = action.group || "Actions";
      if (!acc[group]) acc[group] = [];
      acc[group].push(action);
      return acc;
    },
    {} as Record<string, CommandAction[]>,
  );

  return (
    <AnimatePresence>
      {isOpen && (
        <>
          {/* Backdrop */}
          <motion.div
            initial={{ opacity: 0 }}
            animate={{ opacity: 1 }}
            exit={{ opacity: 0 }}
            transition={{ duration: 0.1 }}
            className="fixed inset-0 bg-black/60 z-50"
            onClick={onClose}
          />

          {/* Command Dialog */}
          <motion.div
            initial={{ opacity: 0, scale: 0.95, y: -20 }}
            animate={{ opacity: 1, scale: 1, y: 0 }}
            exit={{ opacity: 0, scale: 0.95, y: -20 }}
            transition={{ duration: 0.15, ease: "easeOut" }}
            className="fixed top-[20%] left-1/2 -translate-x-1/2 w-full max-w-xl z-50"
          >
            <Command
              className="bg-[#0d0d0d] border border-[#262626] rounded-lg shadow-2xl overflow-hidden"
              loop
            >
              {/* Search input */}
              <div className="flex items-center gap-3 px-4 py-3 border-b border-[#1c1c1c]">
                <Search size={18} className="text-[#555555]" />
                <Command.Input
                  value={search}
                  onValueChange={setSearch}
                  placeholder="Type a command or search..."
                  className="flex-1 bg-transparent text-[#ededed] placeholder-[#555555] outline-none text-sm"
                  autoFocus
                />
                <kbd className="px-1.5 py-0.5 text-xs text-[#555555] bg-[#1c1c1c] rounded">
                  ESC
                </kbd>
              </div>

              {/* Results */}
              <Command.List className="max-h-80 overflow-y-auto p-2">
                <Command.Empty className="py-6 text-center text-sm text-[#555555]">
                  No results found.
                </Command.Empty>

                {/* Files group */}
                {files.length > 0 && (
                  <Command.Group
                    heading={
                      <span className="text-xs text-[#555555] font-medium px-2">
                        Files
                      </span>
                    }
                  >
                    {files.map((file) => (
                      <Command.Item
                        key={file.filename}
                        value={`file ${file.filename}`}
                        onSelect={() => {
                          onSelectFile?.(file.filename);
                          onClose();
                        }}
                        className={clsx(
                          "flex items-center gap-3 px-3 py-2 rounded cursor-pointer",
                          "text-sm text-[#ededed]",
                          "aria-selected:bg-[#5e6ad2]/20",
                          "hover:bg-[#1c1c1c]",
                        )}
                      >
                        <File size={14} className="text-[#888888]" />
                        <span className="flex-1 truncate font-mono text-xs">
                          {file.filename}
                        </span>
                        {file.isViewed && (
                          <Check size={14} className="text-[#27a300]" />
                        )}
                      </Command.Item>
                    ))}
                  </Command.Group>
                )}

                {/* Action groups */}
                {Object.entries(groupedActions).map(([group, groupActions]) => (
                  <Command.Group
                    key={group}
                    heading={
                      <span className="text-xs text-[#555555] font-medium px-2">
                        {group}
                      </span>
                    }
                  >
                    {groupActions.map((action) => (
                      <Command.Item
                        key={action.id}
                        value={action.label}
                        onSelect={() => {
                          action.onSelect();
                          onClose();
                        }}
                        className={clsx(
                          "flex items-center gap-3 px-3 py-2 rounded cursor-pointer",
                          "text-sm text-[#ededed]",
                          "aria-selected:bg-[#5e6ad2]/20",
                          "hover:bg-[#1c1c1c]",
                        )}
                      >
                        {action.icon || (
                          <Keyboard size={14} className="text-[#888888]" />
                        )}
                        <span className="flex-1">{action.label}</span>
                        {action.shortcut && (
                          <kbd className="px-1.5 py-0.5 text-xs text-[#555555] bg-[#1c1c1c] rounded font-mono">
                            {action.shortcut}
                          </kbd>
                        )}
                      </Command.Item>
                    ))}
                  </Command.Group>
                ))}
              </Command.List>
            </Command>
          </motion.div>
        </>
      )}
    </AnimatePresence>
  );
}

// Hook to manage command palette state and keyboard shortcuts
export function useCommandPalette() {
  const [isOpen, setIsOpen] = useState(false);

  const open = useCallback(() => setIsOpen(true), []);
  const close = useCallback(() => setIsOpen(false), []);
  const toggle = useCallback(() => setIsOpen((prev) => !prev), []);

  // Global Cmd+K / Ctrl+K handler
  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key === "k") {
        e.preventDefault();
        toggle();
      }
    };

    document.addEventListener("keydown", handleKeyDown);
    return () => document.removeEventListener("keydown", handleKeyDown);
  }, [toggle]);

  return { isOpen, open, close, toggle };
}
