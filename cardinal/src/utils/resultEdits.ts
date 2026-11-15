import type { ResultEdit } from '../types/ipc';
import type { SlabIndex } from '../types/slab';

// Edits refer to one native revision, in ascending original coordinates.
// Copy unchanged ranges once, even for a burst with many insertions.
export function applyResultEdits(source: SlabIndex[], edits: readonly ResultEdit[]): SlabIndex[] {
  if (!edits.length) return source;
  let length = source.length;
  let end = 0;
  for (const { at, remove, insert } of edits) {
    if (at < end || remove < 0 || at + remove > source.length) {
      throw new Error('Invalid result edit for this view');
    }
    end = at + remove;
    length += insert.length - remove;
  }
  const next = new Array<SlabIndex>(length);
  let from = 0;
  let to = 0;
  for (const { at, remove, insert } of edits) {
    while (from < at) next[to++] = source[from++];
    for (const id of insert) next[to++] = id as SlabIndex;
    from += remove;
  }
  while (from < source.length) next[to++] = source[from++];
  return next;
}
