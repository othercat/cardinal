import { useCallback } from 'react';
import type { MouseEvent as ReactMouseEvent } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { Menu } from '@tauri-apps/api/menu';
import type { MenuItemOptions, MenuOptions } from '@tauri-apps/api/menu';
import { useTranslation } from 'react-i18next';
import { openResultPath } from '../utils/openResultPath';
import { splitPath } from '../utils/path';
import { formatPathRule, isBelow, parentFolders } from '../utils/pathExclusions';

type MenuItems = NonNullable<MenuOptions['items']>;
type ExclusionTarget = {
  path: string;
  isDirectory?: boolean;
  root: string;
  exclude: (path: string) => void;
};

type UseContextMenuResult = {
  showContextMenu: (
    event: ReactMouseEvent<HTMLElement>,
    targetPaths: string[],
    target?: ExclusionTarget,
  ) => void;
  showHeaderContextMenu: (event: ReactMouseEvent<HTMLElement>) => void;
};

export function useContextMenu(
  autoFitColumns: (() => void) | null = null,
  onQuickLookRequest?: () => void | Promise<void>,
  volume?: { online: boolean; open: (path: string, reveal?: boolean) => void },
): UseContextMenuResult {
  const { t } = useTranslation();
  const writeClipboard = useCallback((text: string) => {
    if (!navigator?.clipboard?.writeText) {
      return;
    }
    void navigator.clipboard.writeText(text);
  }, []);

  const buildFileMenuItems = useCallback(
    (targetPathsInput: string[], target?: ExclusionTarget): MenuItems => {
      const targetPaths = targetPathsInput.filter(Boolean);
      if (targetPaths.length === 0) {
        return [];
      }
      const copyLabel =
        targetPaths.length > 1 ? t('contextMenu.copyFiles') : t('contextMenu.copyFile');
      const copyFilenameLabel =
        targetPaths.length > 1 ? t('contextMenu.copyFilenames') : t('contextMenu.copyFilename');
      const copyPathLabel =
        targetPaths.length > 1 ? t('contextMenu.copyPaths') : t('contextMenu.copyPath');
      const items: MenuItems = [
        {
          id: 'context_menu.open_item',
          text: t('contextMenu.openItem'),
          enabled: volume?.online ?? true,
          accelerator: 'Cmd+O',
          action: () => {
            targetPaths.forEach((path) => (volume ? volume.open(path) : openResultPath(path)));
          },
        },
        {
          id: 'context_menu.open_in_finder',
          text: t('contextMenu.revealInFinder'),
          enabled: volume?.online ?? true,
          accelerator: 'Cmd+R',
          action: () => {
            targetPaths.forEach((itemPath) => {
              if (volume) volume.open(itemPath, true);
              else void invoke('open_in_finder', { path: itemPath });
            });
          },
        },
        {
          id: 'context_menu.copy_filename',
          text: copyFilenameLabel,
          action: () => {
            const filenames = targetPaths
              .map((itemPath) => splitPath(itemPath).name || itemPath)
              .join(' ');
            writeClipboard(filenames);
          },
        },
        {
          id: 'context_menu.copy_paths',
          text: copyPathLabel,
          accelerator: 'Cmd+Shift+C',
          action: () => {
            writeClipboard(targetPaths.join('\n'));
          },
        },
      ];
      if (!volume)
        items.push({
          id: 'context_menu.copy_files',
          text: copyLabel,
          accelerator: 'Cmd+C',
          action: () => {
            void invoke('copy_files_to_clipboard', { paths: targetPaths }).catch((error) => {
              console.error('Failed to copy files to clipboard', error);
            });
          },
        });

      if (onQuickLookRequest) {
        items.push({
          id: 'context_menu.quicklook',
          text: t('contextMenu.quickLook'),
          accelerator: 'Space',
          action: () => {
            void onQuickLookRequest();
          },
        });
      }

      if (target && isBelow(target.path, target.root)) {
        const { path, isDirectory, exclude } = target;
        items.push(
          { item: 'Separator' },
          {
            id: 'context_menu.exclude_item',
            text: t(
              isDirectory === undefined
                ? 'contextMenu.excludePath'
                : isDirectory
                  ? 'contextMenu.excludeDirectory'
                  : 'contextMenu.excludeFile',
            ),
            action: () => exclude(path),
          },
        );
        const parents = parentFolders(path, target.root);
        if (parents.length)
          items.push({
            id: 'context_menu.exclude_parent',
            text: t('contextMenu.excludeParent'),
            items: parents.map((folder, i) => ({
              id: `context_menu.exclude_parent_${i}`,
              text: formatPathRule(folder).replace(/&/g, '&&'),
              action: () => exclude(folder),
            })),
          });
      }

      return items;
    },
    [onQuickLookRequest, t, volume, writeClipboard],
  );

  const buildHeaderMenuItems = useCallback((): MenuItemOptions[] => {
    if (!autoFitColumns) {
      return [];
    }

    return [
      {
        id: 'context_menu.reset_column_widths',
        text: t('contextMenu.resetColumnWidths'),
        action: () => {
          autoFitColumns();
        },
      },
    ];
  }, [autoFitColumns, t]);

  const showMenu = useCallback(async (items: MenuItems) => {
    if (!items.length) {
      return;
    }

    try {
      const menu = await Menu.new({ items });
      try {
        await menu.popup();
      } finally {
        await menu.close();
      }
    } catch (error) {
      console.error('Failed to show context menu', error);
    }
  }, []);

  const showContextMenu = useCallback(
    (event: ReactMouseEvent<HTMLElement>, targetPaths: string[], target?: ExclusionTarget) => {
      event.preventDefault();
      event.stopPropagation();
      void showMenu(buildFileMenuItems(targetPaths, target));
    },
    [buildFileMenuItems, showMenu],
  );

  const showHeaderContextMenu = useCallback(
    (event: ReactMouseEvent<HTMLElement>) => {
      event.preventDefault();
      event.stopPropagation();
      void showMenu(buildHeaderMenuItems());
    },
    [buildHeaderMenuItems, showMenu],
  );

  return {
    showContextMenu,
    showHeaderContextMenu,
  };
}
