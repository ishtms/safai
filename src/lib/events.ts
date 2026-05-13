// shared Tauri event envelope. Rust emits this shape for every streaming
// channel so screens can filter by handle and ignore stale delivery.

export interface IpcEventEnvelope<T> {
  kind: string;
  handleId: string;
  phase: string | null;
  payload: T;
  sequence: number;
  terminal: boolean;
}

export function createEnvelopeGate(handleId: string) {
  let lastSequence = 0;
  return function accept<T>(
    ev: IpcEventEnvelope<T>,
    handler: (payload: T, envelope: IpcEventEnvelope<T>) => void,
  ) {
    if (ev.handleId !== handleId) return;
    if (!Number.isFinite(ev.sequence) || ev.sequence <= lastSequence) return;
    lastSequence = ev.sequence;
    handler(ev.payload, ev);
  };
}
