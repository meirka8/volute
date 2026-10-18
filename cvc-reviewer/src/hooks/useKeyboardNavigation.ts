import { useEffect, useCallback } from 'react';

/**
 * Things that already own the Space key when they have focus: Space activates a
 * button, a link, a summary or a select, and scrolls or acts within anything else
 * that is focusable (a card that selects itself, a block of code that scrolls).
 * The page-wide Space shortcut must leave all of them alone: cancelling Space on
 * a focused button stops it from activating, and marking the open file viewed as
 * a side effect of operating a control is a surprise.
 */
const OWNS_SPACE = 'button, a[href], summary, select, [role="button"], [tabindex]:not([tabindex="-1"])';

interface KeyboardNavigationOptions {
  onNext: () => void;          // j key
  onPrevious: () => void;      // k key
  onExpand: () => void;        // x key
  onMarkViewed: () => void;    // Space key
  enabled?: boolean;
}

export function useKeyboardNavigation({
  onNext,
  onPrevious,
  onExpand,
  onMarkViewed,
  enabled = true,
}: KeyboardNavigationOptions) {
  const handleKeyDown = useCallback(
    (e: KeyboardEvent) => {
      // Don't handle if user is typing in an input
      const target = e.target as HTMLElement;
      if (
        target.tagName === 'INPUT' ||
        target.tagName === 'TEXTAREA' ||
        target.isContentEditable
      ) {
        return;
      }

      // Don't handle if modifier keys are pressed (except for specific combos)
      if (e.metaKey || e.ctrlKey || e.altKey) {
        return;
      }

      switch (e.key.toLowerCase()) {
        case 'j':
          e.preventDefault();
          onNext();
          break;
        case 'k':
          e.preventDefault();
          onPrevious();
          break;
        case 'x':
          e.preventDefault();
          onExpand();
          break;
        case ' ':
          if (target.closest?.(OWNS_SPACE)) return;
          e.preventDefault();
          onMarkViewed();
          break;
      }
    },
    [onNext, onPrevious, onExpand, onMarkViewed]
  );

  useEffect(() => {
    if (!enabled) return;

    document.addEventListener('keydown', handleKeyDown);
    return () => document.removeEventListener('keydown', handleKeyDown);
  }, [handleKeyDown, enabled]);
}

// Helper hook for managing file navigation state
export function useFileNavigation(files: string[]) {
  const getNextFile = useCallback(
    (currentFile: string | null): string | null => {
      if (files.length === 0) return null;
      if (!currentFile) return files[0];

      const currentIndex = files.indexOf(currentFile);
      if (currentIndex === -1) return files[0];
      if (currentIndex >= files.length - 1) return files[0]; // wrap around

      return files[currentIndex + 1];
    },
    [files]
  );

  const getPreviousFile = useCallback(
    (currentFile: string | null): string | null => {
      if (files.length === 0) return null;
      if (!currentFile) return files[files.length - 1];

      const currentIndex = files.indexOf(currentFile);
      if (currentIndex === -1) return files[files.length - 1];
      if (currentIndex <= 0) return files[files.length - 1]; // wrap around

      return files[currentIndex - 1];
    },
    [files]
  );

  return { getNextFile, getPreviousFile };
}
