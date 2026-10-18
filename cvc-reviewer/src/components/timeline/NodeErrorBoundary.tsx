import { Component, type ReactNode } from 'react';
import type { InteractionNode } from '../../types/cvc';

interface NodeErrorBoundaryProps {
  interaction: InteractionNode;
  children: ReactNode;
}

interface NodeErrorBoundaryState {
  failed: boolean;
}

/**
 * Keeps one step that cannot be drawn from taking the whole timeline down with
 * it. A node can hold anything (blobs from before the format was validated, text
 * of any shape), and a render error in one card would otherwise unmount every
 * card. The fallback is a small card naming the step by its id.
 *
 * It renders no element of its own around a healthy step, so the card's markup is
 * exactly what it would be without it.
 */
export class NodeErrorBoundary extends Component<NodeErrorBoundaryProps, NodeErrorBoundaryState> {
  state: NodeErrorBoundaryState = { failed: false };

  static getDerivedStateFromError(): NodeErrorBoundaryState {
    return { failed: true };
  }

  componentDidUpdate(previous: NodeErrorBoundaryProps) {
    // A step whose data is replaced (a link arrived, a refetch) gets another try.
    if (this.state.failed && previous.interaction !== this.props.interaction) {
      this.setState({ failed: false });
    }
  }

  render() {
    if (!this.state.failed) return this.props.children;
    const id = this.props.interaction?.id;
    return (
      <div className="mx-3 my-3 rounded-[1.5rem] border border-line bg-surface/55 p-4 text-sm text-muted">
        <p>This step could not be displayed.</p>
        {typeof id === 'string' && <p className="mt-1 break-all font-mono text-xs">{id}</p>}
      </div>
    );
  }
}
