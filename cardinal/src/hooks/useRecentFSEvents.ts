import { useEffect, useMemo, useReducer, useRef } from 'react';
import { subscribeFSEventsBatch } from '../runtime/tauriEventRuntime';
import type { RecentEventPayload } from '../types/ipc';

// Listen to batched file-system events and expose filtered projections for the UI.
const MAX_EVENTS = 10000;

type RecentEventsOptions = {
  caseSensitive: boolean;
  isActive: boolean;
  eventFilterQuery: string;
};

export function useRecentFSEvents({
  caseSensitive,
  isActive,
  eventFilterQuery,
}: RecentEventsOptions) {
  const eventsRef = useRef<RecentEventPayload[]>([]);
  const nextSlot = useRef(0);
  const isActiveRef = useRef(isActive);
  const [bufferVersion, bumpBufferVersion] = useReducer((count: number) => count + 1, 0);

  useEffect(() => {
    isActiveRef.current = isActive;
    if (isActive) {
      bumpBufferVersion();
    }
  }, [isActive]);

  useEffect(() => {
    const unlistenEvents = subscribeFSEventsBatch((payload: RecentEventPayload[]) => {
      if (payload.length === 0) return;

      for (const event of payload) {
        eventsRef.current[nextSlot.current] = event;
        nextSlot.current = (nextSlot.current + 1) % MAX_EVENTS;
      }

      if (isActiveRef.current) {
        bumpBufferVersion();
      }
    });
    return unlistenEvents;
  }, []);

  const filteredEvents = useMemo(() => {
    if (!isActive) return [];
    const buffer = eventsRef.current;
    const start = buffer.length === MAX_EVENTS ? nextSlot.current : 0;
    const events = start ? buffer.slice(start).concat(buffer.slice(0, start)) : buffer.slice();
    const query = eventFilterQuery.trim();
    if (!query) return events;
    const needle = caseSensitive ? query : query.toLowerCase();
    return events.filter(({ path }) =>
      (caseSensitive ? path : path.toLowerCase()).includes(needle),
    );
  }, [isActive, eventFilterQuery, caseSensitive, bufferVersion]);

  return {
    filteredEvents,
  };
}
