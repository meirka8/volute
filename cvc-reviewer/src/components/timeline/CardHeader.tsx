import type { ReactNode } from 'react';

/**
 * What the header of a card needs from the card to select it and to name it.
 * The card (`TimelineNode`) makes these; both layouts' headers use them.
 */
export interface CardSelection {
  /** Whether the step is the selected one. */
  pressed: boolean;
  /** Selects the step. */
  onSelect?: () => void;
  /** The id of the speaker label in the header: the card is named by it... */
  speakerId: string;
  /** ...and by the id of the time. Together they are the short name of the card in a list of thousands. */
  timeId: string;
}

interface CardHeaderProps {
  selection: CardSelection;
  /** The icon for who spoke. It is decoration (the icons are hidden from assistive technology), since the label says it in words. */
  icon: ReactNode;
  /** Who spoke ("AI step", "AI Agent", "User", ...). */
  speaker: ReactNode;
  /** Chips between the speaker and the time (the stimulus, how many calls failed, the signal). */
  badges?: ReactNode;
  /** When the step happened, as text. */
  time: ReactNode;
  /** The model, when the capture says which. */
  model?: ReactNode;
}

/**
 * The header of a card, and the one control that selects it for a person using a
 * keyboard or a screen reader.
 *
 * The card is not a button. A button cannot hold other buttons (what is inside it
 * is read as part of it and not as controls, and its name is all of its text), and
 * a card holds several: the toggles that open tool results, code and long text.
 * So the card is a plain list item named by this header's speaker and time, and
 * the header is a real toggle button: Tab reaches it, Enter or Space selects, and
 * `aria-pressed` says whether the step is the selected one. A click anywhere else
 * on the card still selects it, for the mouse (see `TimelineNode`).
 *
 * It is made of spans only, because a button may hold nothing but phrasing content.
 * Its parts are separated by spaces in the text, not only by the layout: the name a
 * button is given is the text it holds, and without the spaces it would run together
 * ("AI stepPrompt9 Mar") wherever the layout is not taken into account.
 */
export function CardHeader({ selection, icon, speaker, badges, time, model }: CardHeaderProps) {
  return (
    <button
      type="button"
      aria-pressed={selection.pressed}
      onClick={(event) => {
        // The header selects on its own account, so Enter and Space always work, even while some text
        // in the card is selected; and the card's click handler, which leaves a selection of text alone,
        // must not also run.
        event.stopPropagation();
        selection.onSelect?.();
      }}
      className="mb-4 flex w-full cursor-pointer items-start gap-3 rounded-xl text-left"
    >
      <span className="flex h-9 w-9 shrink-0 items-center justify-center rounded-full bg-canvas/80">{icon}</span>
      <span className="min-w-0 flex-1">
        <span className="flex flex-wrap items-center gap-2">
          <span id={selection.speakerId} className="font-medium text-ink">
            {speaker}
          </span>{' '}
          {badges}
          <span id={selection.timeId} className="text-xs text-muted">
            {time}
          </span>
        </span>
        {model ? (
          <>
            {' '}
            <span className="mt-1 block break-words text-xs uppercase tracking-[0.16em] text-muted">{model}</span>
          </>
        ) : null}
      </span>
    </button>
  );
}
