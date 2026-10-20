import { createContext, useCallback, useContext, useLayoutEffect, useMemo, useState, type ReactNode } from 'react';
import { AlertTriangle } from 'lucide-react';
import { hiddenCodePointLabel, hiddenTitle, hiddenToken, splitHidden, type HiddenMode } from '../../lib/hiddenChars';

/**
 * Hidden and bidirectional characters, drawn (issue #135). See `lib/hiddenChars`
 * for the class and the two levels (`strict` for code-like text, `prose` for text
 * a person or the model wrote).
 *
 * A card wraps its contents in a `HiddenCharsScope`. Every `HiddenText` that has a
 * hidden character to draw tells the scope, and the card's `HiddenCharsNotice`
 * shows while any of them is on screen. So the notice's sentence ("shown as
 * ⟨U+…⟩") is true whenever it is there, and text that is not drawn (a folded block,
 * a list not yet paged in) does not raise it.
 */

/** How a `HiddenText` tells its card that it is drawing a hidden character, and that it stopped. */
const ReportContext = createContext<((delta: 1 | -1) => void) | null>(null);
/** How many `HiddenText`s of the card are drawing one. */
const CountContext = createContext(0);

/** A warning-tinted monospace chip in the text, in the page's ink colour so it reads on both themes. */
const TOKEN_CLASS =
  'mx-px whitespace-nowrap rounded border border-warning/60 bg-warning/25 px-1 font-mono text-[0.85em] font-medium text-ink';

/**
 * Text with its hidden characters drawn as visible tokens. Text without any is
 * returned as it is, so it draws exactly as a plain string would: no wrapper, no
 * extra elements.
 */
export function HiddenText({ text, mode }: { text: string; mode: HiddenMode }) {
  // Worked out once per text. A value that is not text is left for React to draw
  // (or to refuse, as it always did).
  const segments = useMemo(() => (typeof text === 'string' ? splitHidden(text, mode) : null), [text, mode]);
  const report = useContext(ReportContext);
  const found = segments !== null;

  useLayoutEffect(() => {
    if (!found || report === null) return;
    report(1);
    return () => report(-1);
  }, [found, report]);

  if (segments === null) return text;
  return (
    <>
      {segments.map((segment, index) => {
        if (segment.kind === 'text') return segment.text;
        if (segment.kind === 'hidden') {
          const title = hiddenTitle(segment.codePoint);
          return (
            <span
              key={index}
              role="img"
              aria-label={title}
              title={title}
              data-hidden-char={hiddenCodePointLabel(segment.codePoint)}
              className={TOKEN_CLASS}
            >
              {hiddenToken(segment.codePoint)}
            </span>
          );
        }
        const summary = `${segment.count.toLocaleString('en-US')} more hidden characters, left out of this text`;
        return (
          <span key={index} role="img" aria-label={summary} title={summary} data-hidden-char="more" className={TOKEN_CLASS}>
            {`⟨+${segment.count.toLocaleString('en-US')} more⟩`}
          </span>
        );
      })}
    </>
  );
}

/** Counts the hidden characters a card is drawing. It adds no element of its own. */
export function HiddenCharsScope({ children }: { children: ReactNode }) {
  const [count, setCount] = useState(0);
  const report = useCallback((delta: 1 | -1) => setCount((current) => current + delta), []);
  return (
    <ReportContext.Provider value={report}>
      <CountContext.Provider value={count}>{children}</CountContext.Provider>
    </ReportContext.Provider>
  );
}

/** One line for the card, shown only while it is drawing a hidden character. */
export function HiddenCharsNotice() {
  const count = useContext(CountContext);
  if (count === 0) return null;
  return (
    <p
      role="note"
      className="mb-3 flex items-start gap-2 rounded-xl border border-warning/50 bg-warning/10 px-3 py-2 text-xs text-ink"
    >
      <AlertTriangle size={14} className="mt-0.5 shrink-0 text-warning" aria-hidden="true" />
      <span>This step contains hidden or bidirectional Unicode characters, shown as ⟨U+…⟩.</span>
    </p>
  );
}
