import { invoke } from '@tauri-apps/api/core';
import type { SlabIndex } from '../types/slab';
import type { SortState } from '../types/sort';

// The native view owns membership and ordering. Keep a bounded number of pages
// in the WebView, independent of whether the index contains thousands or millions.
export class ResultPages {
  private readonly cached = new Map<number, SlabIndex[]>();
  private readonly pending = new Map<number, Promise<SlabIndex[]>>();
  private readonly size: number;

  constructor(
    readonly viewId: number,
    readonly length: number,
    private readonly sort: SortState,
    first: SlabIndex[],
  ) {
    this.size = 256;
    if (first.length === Math.min(this.size, length)) this.cached.set(0, first);
  }

  idAt(index: number): SlabIndex | undefined {
    return this.cached.get(Math.floor(index / this.size))?.[index % this.size];
  }

  private page(number: number): Promise<SlabIndex[]> {
    const cached = this.cached.get(number);
    if (cached) return Promise.resolve(cached);
    const existing = this.pending.get(number);
    if (existing) return existing;
    const offset = number * this.size;
    const request = invoke<SlabIndex[]>('get_view_page', {
      viewId: this.viewId,
      sort: this.sort,
      offset,
      limit: this.size,
    })
      .then((ids) => {
        if (ids.length !== Math.min(this.size, this.length - offset))
          throw new Error('File view changed during paging');
        this.cached.set(number, ids);
        if (this.cached.size > 64) this.cached.delete(this.cached.keys().next().value!);
        return ids;
      })
      .finally(() => this.pending.delete(number));
    this.pending.set(number, request);
    return request;
  }

  async range(start: number, end: number): Promise<SlabIndex[]> {
    const lo = Math.max(0, start),
      hi = Math.min(this.length, end);
    if (lo >= hi) return [];
    const first = Math.floor(lo / this.size);
    const last = Math.floor((hi - 1) / this.size);
    const pages = await Promise.all(
      Array.from({ length: last - first + 1 }, (_, i) => this.page(first + i)),
    );
    return pages.flat().slice(lo % this.size, (lo % this.size) + hi - lo);
  }

  async positions(ids: SlabIndex[]): Promise<Map<SlabIndex, number>> {
    const ranks = await invoke<number[]>('get_view_page', {
      viewId: this.viewId,
      sort: this.sort,
      offset: 0,
      limit: 0,
      locate: ids,
    });
    return new Map(
      ids.flatMap((id, i) => (ranks[i] < this.length ? [[id, ranks[i]] as const] : [])),
    );
  }
}
