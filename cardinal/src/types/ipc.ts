import type { SlabIndex } from './slab';
import type { SortKey } from './sort';

export type MetadataProgress = { processed: number; total: number; revision: number };

export type StatusBarUpdatePayload = {
  metadata?: MetadataProgress;
  scannedFiles: number;
  processedEvents: number;
  rescanErrors: number;
};

export type IconUpdateWirePayload = {
  slabIndex: number;
  path?: string;
  icon?: string;
  thumbnail?: boolean;
};

export type IconUpdatePayload = {
  slabIndex: SlabIndex;
  path?: string;
  icon?: string;
  thumbnail?: boolean;
};

export type RecentEventPayload = {
  path: string;
  flagBits: number;
  eventId: number;
  timestamp: number;
};

// null requests a full refresh after startup/rescan or a bounded queue overflow.
export type FileChangeBatch = readonly RecentEventPayload[] | null;

export type AppLifecycleStatus = 'Initializing' | 'LoadingIndex' | 'Updating' | 'Ready';

export enum SearchStatusCode {
  OK = 0,
  CANCELLED = 1,
}

export type ResultEdit = { at: number; remove: number; insert: number[] };
export type SortedEdits = { baseViewId: number; key: SortKey; edits: ResultEdit[] };
export type SearchResponsePayload = {
  totalCount?: number;
  viewId?: number;
  incremental?: boolean;
  edits?: ResultEdit[];
  sorted?: SortedEdits;
  changedPaths?: string[];
  changedIds?: number[];
  results: number[];
  highlights?: string[];
  statusCode: SearchStatusCode;
};
