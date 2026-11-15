import type { SlabIndex } from '../types/slab';
import type { SortKey } from '../types/sort';
import type { SearchResultItem } from '../types/search';

export type LoadedWindow = { start: number; rows: { id: SlabIndex; item: SearchResultItem }[] };

// Rust compares UTF-8 strings by Unicode scalar value. JS compares UTF-16
// units, which differ in order for supplementary characters versus high BMP.
function compareText(a: string, b: string): number {
  if (a === b) return 0;
  if (!/[\uD800-\uDFFF]/.test(a) && !/[\uD800-\uDFFF]/.test(b)) return a < b ? -1 : 1;
  const right = b[Symbol.iterator]();
  for (const ch of a) {
    const next = right.next();
    if (next.done) return 1;
    const diff = ch.codePointAt(0)! - next.value.codePointAt(0)!;
    if (diff) return diff;
  }
  return -1;
}

export function sortedWindow(
  window: LoadedWindow,
  key: SortKey,
  descending: boolean,
): SlabIndex[] | null {
  const entries = window.rows.map(({ id, item }) => {
    const { path, metadata } = item;
    const name = path.slice(path.lastIndexOf('/') + 1) || '/';
    const kind = metadata ? (metadata.type === 1 ? 0 : 1) : 2;
    const numeric =
      key === 'size'
        ? metadata?.type === 1
          ? -1
          : metadata?.allocatedSize
        : key === 'mtime'
          ? item.mtime || undefined
          : item.ctime || undefined;
    return { id, path, name, kind, numeric: numeric ?? -Infinity };
  });
  if (key === 'size' && entries.some((row) => row.numeric >= Number.MAX_SAFE_INTEGER)) return null;
  entries.sort((a, b) => {
    let cmp = 0;
    if (key !== 'filename' && key !== 'fullPath') {
      cmp = a.numeric < b.numeric ? -1 : a.numeric > b.numeric ? 1 : 0;
    }
    if (!cmp && key !== 'fullPath') cmp = compareText(a.name, b.name) || a.kind - b.kind;
    cmp ||= compareText(a.path, b.path) || a.id - b.id;
    return descending ? -cmp : cmp;
  });
  return entries.map((row) => row.id);
}
