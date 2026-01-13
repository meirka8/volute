import { useEffect, useCallback } from 'react';

interface KeyboardNavigationOptions {
  onNext: () => void;          // j key
  onPrevious: () => void;      // k key
  onExpand: () => void;        // x key
  onMarkViewed: () => void;    // Space key
  onOpenPalette: () => void;   // Cmd+K (handled separately but included for reference)
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
