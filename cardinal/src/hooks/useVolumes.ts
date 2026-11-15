import { useCallback, useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';

export type VolumeInfo = {
  id: string;
  path: string;
  format: string;
  online: boolean;
  indexing: boolean;
  reusable: boolean;
  revision: number;
  count: number | null;
  error: string | null;
};

const key = 'cardinal.selectedVolume';
export function useVolumes() {
  const [volumes, setVolumes] = useState<VolumeInfo[]>([]);
  const [id, setId] = useState<string | null>(() => {
    try {
      return localStorage.getItem(key);
    } catch {
      return null;
    }
  });
  useEffect(() => {
    if (!('__TAURI_INTERNALS__' in window)) return;
    let alive = true;
    let sequence = 0;
    const refresh = async () => {
      const request = ++sequence;
      const next = await invoke<VolumeInfo[]>('list_volumes');
      if (alive && sequence === request && Array.isArray(next)) setVolumes(next);
    };
    const subscription = listen('volumes_changed', () => {
      void refresh().catch(console.error);
    });
    void subscription.then(refresh).catch(console.error);
    return () => {
      alive = false;
      void subscription.then((off) => off()).catch(console.error);
    };
  }, []);
  const select = useCallback((volume: VolumeInfo | null) => {
    setId(volume?.id ?? null);
    try {
      if (volume) localStorage.setItem(key, volume.id);
      else localStorage.removeItem(key);
    } catch {
      /* Selection still works for this window. */
    }
  }, []);
  return { volumes, selected: volumes.find((v) => v.id === id) ?? null, select };
}
