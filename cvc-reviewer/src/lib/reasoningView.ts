import { readReasoning } from './reasoningMarker';
import { withheldFieldBytes } from './transcriptNode';

/** The label of reasoning a verified marker says is the provider's summary. Never "full" or "complete" reasoning. */
export const PROVIDER_SUMMARY_LABEL = "Provider summary of the model's reasoning";
/** Today's label of reasoning that carries no verified marker. */
export const REASONING_LABEL = 'Reasoning';
/** The label of a field the capture could not scrub and did not record. */
export const WITHHELD_LABEL = 'Withheld';

/** What a card shows for the `model_cot` of a step. */
export type ReasoningDisplay =
  /** The capture withheld it: only how many bytes. */
  | { readonly kind: 'withheld'; readonly label: typeof WITHHELD_LABEL; readonly bytes: bigint }
  /** A verified marker says it is the provider's summary; the text is the body, marker stripped. */
  | { readonly kind: 'provider_summary'; readonly label: typeof PROVIDER_SUMMARY_LABEL; readonly text: string }
  /** Anything else: the text, as it is, as reasoning the agent exposed. */
  | { readonly kind: 'exposed'; readonly label: typeof REASONING_LABEL; readonly text: string };

/**
 * How to show the `model_cot` of node `id`. A verified marker is read first: the body of a verified summary is
 * text, even when it reads like the withheld note (the writer never marks a note, and a reader that took the
 * words for one would hide what the agent recorded). Otherwise exactly the withheld note is "Withheld", and
 * every other text is shown literally under today's label.
 */
export function reasoningDisplay(id: string, modelCot: string): ReasoningDisplay {
  const reading = readReasoning(id, modelCot);
  if (reading.kind === 'provider_summary') return { kind: 'provider_summary', label: PROVIDER_SUMMARY_LABEL, text: reading.body };
  const bytes = withheldFieldBytes(reading.text);
  if (bytes !== null) return { kind: 'withheld', label: WITHHELD_LABEL, bytes };
  return { kind: 'exposed', label: REASONING_LABEL, text: reading.text };
}
