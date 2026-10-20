/**
 * Text as it should be displayed: Windows (`\r\n`) and old Mac (`\r`) line
 * endings become `\n`, so a stray carriage return never shows and lines are
 * counted the same way everywhere. Tabs and all other characters are untouched.
 */
export function displayText(text: string): string {
  // Split and join rather than a regex: plain linear scans, whatever the text holds.
  return text.includes('\r') ? text.split('\r\n').join('\n').split('\r').join('\n') : text;
}

/**
 * The lines of a block of text as it should be displayed: split on `\n`, with
 * the single trailing newline most command output ends with ignored so it does
 * not count as an extra empty line.
 */
export function splitDisplayLines(text: string): string[] {
  const normalized = displayText(text);
  return (normalized.endsWith('\n') ? normalized.slice(0, -1) : normalized).split('\n');
}

/**
 * Whether `text` has more than `limit` lines. Stops scanning as soon as the
 * answer is known, so it is cheap on large text.
 */
export function exceedsLines(text: string, limit: number): boolean {
  let lines = 1;
  for (let at = text.indexOf('\n'); at !== -1; at = text.indexOf('\n', at + 1)) {
    if (++lines > limit) return true;
  }
  return false;
}

/** Hiding fewer lines than this is not worth a "Show all" click. */
const MIN_HIDDEN_LINES = 3;

/**
 * Whether a block of `total` lines should be cut to `shown` lines behind a
 * "Show all N lines" control. A block that is only a line or two over the limit
 * is shown whole: the control would hide less than it costs to find.
 */
export function shouldCollapse(total: number, shown: number): boolean {
  return total - shown >= MIN_HIDDEN_LINES;
}

/**
 * How many characters of a block that WRAPS are shown while it is collapsed.
 * Lines alone do not bound such a block: it grows with its text, so a single
 * enormous line (minified output, a base64 blob) becomes thousands of screen
 * rows that push everything after it out of sight. A wrapped row holds a few
 * dozen characters, so this is about a screen at the narrowest pane.
 */
export const WRAPPED_CHAR_BUDGET = 1500;

/** Hiding fewer characters than this is not worth a "Show all" click. */
const MIN_HIDDEN_CHARS = 500;

/**
 * Whether a wrapping block of `length` characters should be cut to
 * WRAPPED_CHAR_BUDGET behind a "Show all N characters" control.
 */
export function exceedsCharBudget(length: number): boolean {
  return length - WRAPPED_CHAR_BUDGET >= MIN_HIDDEN_CHARS;
}

/**
 * The first `limit` characters of `text`, never ending halfway through a surrogate
 * pair. A limit of zero or less keeps nothing (it is not read as "from the end").
 */
export function cutAtCodePoint(text: string, limit: number): string {
  if (limit <= 0) return '';
  if (text.length <= limit) return text;
  const last = text.charCodeAt(limit - 1);
  return text.slice(0, last >= 0xd800 && last <= 0xdbff ? limit - 1 : limit);
}
