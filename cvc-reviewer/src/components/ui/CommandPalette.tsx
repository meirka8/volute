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
  const handleClose = useCallback(() => {
    setSearch("");
    onClose();
  }, [onClose]);

  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        handleClose();
      }
    };

    if (isOpen) {
      document.addEventListener("keydown", handleKeyDown);
      return () => document.removeEventListener("keydown", handleKeyDown);
    }
  }, [handleClose, isOpen]);

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
            className="rr-overlay fixed inset-0 z-50 backdrop-blur-sm"
            onClick={handleClose}
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
              className="rr-panel overflow-hidden rounded-[1.75rem] shadow-2xl"
              loop
            >
              <div className="flex items-center gap-3 border-b border-line/80 px-4 py-3">
                <Search size={18} className="text-muted" />
                <Command.Input
                  value={search}
                  onValueChange={setSearch}
                  placeholder="Type a command or search..."
                  className="flex-1 bg-transparent text-sm text-ink placeholder:text-muted outline-none"
                  autoFocus
                />
                <kbd className="rounded-full bg-canvas/80 px-2 py-1 text-xs text-muted">
                  ESC
                </kbd>
              </div>

              <Command.List className="max-h-80 overflow-y-auto p-2">
                <Command.Empty className="py-6 text-center text-sm text-muted">
                  No results found.
                </Command.Empty>

                {files.length > 0 && (
                  <Command.Group
                    heading={
                      <span className="px-2 text-xs font-medium uppercase tracking-[0.18em] text-muted">
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
                          handleClose();
                        }}
                        className={clsx(
                          "flex cursor-pointer items-center gap-3 rounded-2xl px-3 py-2",
                          "text-sm text-ink",
                          "aria-selected:bg-action/10",
                          "hover:bg-canvas/80",
                        )}
                      >
                        <File size={14} className="text-muted" />
                        <span className="flex-1 truncate font-mono text-xs">
                          {file.filename}
                        </span>
                        {file.isViewed && (
                          <Check size={14} className="text-success" />
                        )}
                      </Command.Item>
                    ))}
                  </Command.Group>
                )}

                {Object.entries(groupedActions).map(([group, groupActions]) => (
                  <Command.Group
                    key={group}
                    heading={
                      <span className="px-2 text-xs font-medium uppercase tracking-[0.18em] text-muted">
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
                          handleClose();
                        }}
                        className={clsx(
                          "flex cursor-pointer items-center gap-3 rounded-2xl px-3 py-2",
                          "text-sm text-ink",
                          "aria-selected:bg-action/10",
                          "hover:bg-canvas/80",
                        )}
                      >
                        {action.icon || (
                          <Keyboard size={14} className="text-muted" />
                        )}
                        <span className="flex-1">{action.label}</span>
                        {action.shortcut && (
                          <kbd className="rounded-full bg-canvas/80 px-2 py-1 text-xs font-mono text-muted">
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
