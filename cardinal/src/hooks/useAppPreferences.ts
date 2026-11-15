import { useCallback, useEffect, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { homeDir } from '@tauri-apps/api/path';
import type { i18n as I18nInstance } from 'i18next';
import { OPEN_PREFERENCES_EVENT } from '../constants/appEvents';
import { getBrowserLanguage } from '../i18n/config';
import { applyThemePreference, persistThemePreference } from '../theme';
import { setTrayEnabled } from '../tray';
import { getStoredTrayIconEnabled, persistTrayIconEnabled } from '../trayIconPreference';
import { setWatchConfig } from '../utils/watchConfig';
import type { FullDiskAccessStatus } from './useFullDiskAccessPermission';
import { useIgnorePaths } from './useIgnorePaths';
import { useIncludePaths } from './useIncludePaths';
import { useWatchRoot } from './useWatchRoot';
import {
  absoluteRule,
  isBelow,
  readVolumeExclusions,
  type VolumeExclusion,
} from '../utils/pathExclusions';
import type { VolumeInfo } from './useVolumes';
import { useStableEvent } from './useStableEvent';
import { useStoredState } from './useStoredState';

type WatchConfigChangePayload = {
  watchRoot: string;
  ignorePaths: string[];
  includePaths: string[];
};

type UseAppPreferencesOptions = {
  fullDiskAccessStatus: FullDiskAccessStatus;
  isCheckingFullDiskAccess: boolean;
  refreshSearchResults: () => void;
  i18n: Pick<I18nInstance, 'changeLanguage'>;
};

type UseAppPreferencesResult = {
  isPreferencesOpen: boolean;
  closePreferences: () => void;
  trayIconEnabled: boolean;
  setTrayIconEnabled: (enabled: boolean) => void;
  watchRoot: string;
  defaultWatchRoot: string;
  ignorePaths: string[];
  defaultIgnorePaths: string[];
  includePaths: string[];
  defaultIncludePaths: string[];
  preferencesResetToken: number;
  handleWatchConfigChange: (next: WatchConfigChangePayload) => void;
  home: string;
  excludePath: (path: string, volume?: Pick<VolumeInfo, 'id' | 'path'>) => void;
  volumeExclusions: VolumeExclusion[];
  setVolumeExclusions: (next: VolumeExclusion[]) => void;
  handleResetPreferences: () => void;
};

const areStringArraysEqual = (left: string[], right: string[]): boolean =>
  left.length === right.length && left.every((value, index) => value === right[index]);

/**
 * Manages app preferences including watch config, tray, theme, language, and overlay state.
 * Provides actions for updating watch settings and resetting preferences to defaults.
 */
export function useAppPreferences({
  fullDiskAccessStatus,
  isCheckingFullDiskAccess,
  refreshSearchResults,
  i18n,
}: UseAppPreferencesOptions): UseAppPreferencesResult {
  const { watchRoot, setWatchRoot, defaultWatchRoot } = useWatchRoot();
  const { ignorePaths, setIgnorePaths, defaultIgnorePaths } = useIgnorePaths();
  const { includePaths, setIncludePaths, defaultIncludePaths } = useIncludePaths();
  const [home, setHome] = useState('');
  const [volumeExclusions, storeVolumeExclusions] = useStoredState<VolumeExclusion[]>({
    key: 'cardinal.volumeExclusions',
    defaultValue: [],
    read: readVolumeExclusions,
    write: JSON.stringify,
    readErrorMessage: 'Unable to read volume exclusions',
    writeErrorMessage: 'Unable to save volume exclusions',
  });
  const volumeRules = useRef(volumeExclusions);
  const setVolumeExclusions = useCallback(
    (next: VolumeExclusion[]) => {
      volumeRules.current = next;
      storeVolumeExclusions(next);
    },
    [storeVolumeExclusions],
  );
  const config = useRef({ watchRoot, ignorePaths, includePaths });
  const pendingConfig = useRef(Promise.resolve());
  const refreshLatest = useStableEvent(refreshSearchResults);
  useEffect(() => {
    if (!('__TAURI_INTERNALS__' in window)) return;
    let alive = true;
    void homeDir()
      .then((dir) => {
        if (alive) setHome(dir);
      })
      .catch(console.error);
    const subscription = listen('watch_config_applied', () => {
      if (alive) refreshLatest();
    });
    return () => {
      alive = false;
      void subscription.then((off) => off()).catch(console.error);
    };
  }, [refreshLatest]);
  const logicStartedRef = useRef(false);
  const [isPreferencesOpen, setIsPreferencesOpen] = useState(false);
  const [trayIconEnabled, setTrayIconEnabled] = useState<boolean>(() => getStoredTrayIconEnabled());
  const [preferencesResetToken, setPreferencesResetToken] = useState(0);

  useEffect(() => {
    persistTrayIconEnabled(trayIconEnabled);
    void setTrayEnabled(trayIconEnabled);
  }, [trayIconEnabled]);

  useEffect(() => {
    if (typeof window === 'undefined') {
      return;
    }

    const handleOpenPreferences = () => setIsPreferencesOpen(true);
    window.addEventListener(OPEN_PREFERENCES_EVENT, handleOpenPreferences);
    return () => window.removeEventListener(OPEN_PREFERENCES_EVENT, handleOpenPreferences);
  }, []);

  useEffect(() => {
    if (isCheckingFullDiskAccess) {
      return;
    }
    if (fullDiskAccessStatus !== 'granted') {
      return;
    }
    if (!watchRoot) {
      return;
    }
    if (logicStartedRef.current) {
      return;
    }

    logicStartedRef.current = true;
    void invoke('start_logic', { watchRoot, ignorePaths, includePaths });
  }, [fullDiskAccessStatus, ignorePaths, includePaths, isCheckingFullDiskAccess, watchRoot]);

  const applyWatchConfig = useCallback(
    (nextWatchRoot: string, nextIgnorePaths: string[], nextIncludePaths: string[]) => {
      const watchConfigChanged =
        nextWatchRoot !== config.current.watchRoot ||
        !areStringArraysEqual(nextIgnorePaths, config.current.ignorePaths) ||
        !areStringArraysEqual(nextIncludePaths, config.current.includePaths);

      if (!watchConfigChanged) {
        return;
      }

      config.current = {
        watchRoot: nextWatchRoot,
        ignorePaths: nextIgnorePaths,
        includePaths: nextIncludePaths,
      };
      setWatchRoot(nextWatchRoot);
      setIgnorePaths(nextIgnorePaths);
      setIncludePaths(nextIncludePaths);
      if (logicStartedRef.current && nextWatchRoot) {
        const next = config.current;
        pendingConfig.current = pendingConfig.current
          .then(() => setWatchConfig(next))
          .catch(console.error);
      }
    },
    [setIgnorePaths, setIncludePaths, setWatchRoot],
  );

  const excludePath = useCallback(
    (path: string, volume?: Pick<VolumeInfo, 'id' | 'path'>) => {
      if (volume) {
        if (!isBelow(path, volume.path)) return;
        const relativePath = path.slice(volume.path.length + 1);
        const existing = volumeRules.current;
        if (
          !existing.some(
            (rule) => rule.volumeId === volume.id && rule.relativePath === relativePath,
          )
        ) {
          setVolumeExclusions([
            ...existing,
            { volumeId: volume.id, mountPath: volume.path, relativePath },
          ]);
        }
        return;
      }
      const current = config.current;
      if (!isBelow(path, absoluteRule(current.watchRoot, home))) return;
      const samePath = (rule: string) => absoluteRule(rule, home) === path;
      applyWatchConfig(
        current.watchRoot,
        current.ignorePaths.some(samePath) ? current.ignorePaths : [...current.ignorePaths, path],
        current.includePaths.filter((rule) => !samePath(rule)),
      );
    },
    [home, applyWatchConfig, setVolumeExclusions],
  );

  const handleWatchConfigChange = useCallback(
    (next: WatchConfigChangePayload) => {
      applyWatchConfig(next.watchRoot, next.ignorePaths, next.includePaths);
    },
    [applyWatchConfig],
  );

  const handleResetPreferences = useCallback(() => {
    setTrayIconEnabled(false);
    persistThemePreference('system');
    applyThemePreference('system');
    const nextLanguage = getBrowserLanguage();
    void i18n.changeLanguage(nextLanguage);
    setPreferencesResetToken((token) => token + 1);
  }, [i18n]);

  const closePreferences = useCallback(() => setIsPreferencesOpen(false), []);

  return {
    isPreferencesOpen,
    closePreferences,
    trayIconEnabled,
    setTrayIconEnabled,
    watchRoot,
    defaultWatchRoot,
    ignorePaths,
    defaultIgnorePaths,
    includePaths,
    defaultIncludePaths,
    preferencesResetToken,
    handleWatchConfigChange,
    home,
    excludePath,
    volumeExclusions,
    setVolumeExclusions,
    handleResetPreferences,
  };
}
