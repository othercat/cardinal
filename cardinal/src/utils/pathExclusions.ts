// A quoted line preserves a real filename that contains a line break or ends in whitespace.
export const formatPathRule = (path: string): string =>
  /[\r\n]|\s$/u.test(path) ? JSON.stringify(path) : path;

export const parsePathRules = (text: string): string[] =>
  text
    .split(/\r?\n/)
    .map((line) => {
      const trimmed = line.trim();
      if (trimmed.startsWith('"')) {
        try {
          const path: unknown = JSON.parse(trimmed);
          if (typeof path === 'string') return path;
        } catch {
          // Leave invalid input visible for the existing absolute-path validation.
        }
      }
      return trimmed;
    })
    .filter(Boolean);

export const cleanStoredPaths = (paths: string[]): string[] =>
  paths
    .map((path) => (path.startsWith('/') || path.startsWith('~/') ? path : path.trim()))
    .filter(Boolean);

export function absoluteRule(path: string, home?: string): string {
  if (home && (path === '~' || path.startsWith('~/'))) path = home + path.slice(1);
  if (!path.startsWith('/')) return '';
  return (
    '/' +
    path
      .split('/')
      .filter((part) => part && part !== '.')
      .join('/')
  );
}

export const isBelow = (path: string, directory: string): boolean =>
  directory !== '' &&
  path !== directory &&
  path.startsWith(directory === '/' ? '/' : directory + '/');

export function parentFolders(path: string, root: string): string[] {
  const parents: string[] = [];
  for (let slash = path.lastIndexOf('/'); slash > 0; slash = path.lastIndexOf('/')) {
    path = path.slice(0, slash);
    if (!isBelow(path, root)) break;
    parents.push(path);
  }
  return parents;
}

// Mirror the walker's longest component-prefix rule, including the route to an explicit include.
export function eventPathFilter(ignored: string[], included: string[], home: string) {
  const ignore = ignored.map((p) => absoluteRule(p, home)).filter(Boolean);
  const include = included.map((p) => absoluteRule(p, home)).filter(Boolean);
  const deepest = (path: string, rules: string[]) =>
    rules.reduce(
      (length, rule) =>
        path === rule || isBelow(path, rule) ? Math.max(length, rule.length) : length,
      0,
    );
  return (path: string) =>
    include.some((rule) => isBelow(rule, path)) || deepest(path, include) >= deepest(path, ignore);
}

export type VolumeExclusion = { volumeId: string; mountPath: string; relativePath: string };

export function readVolumeExclusions(raw: string): VolumeExclusion[] | null {
  const parsed: unknown = JSON.parse(raw);
  if (!Array.isArray(parsed)) return null;
  return parsed.filter(
    (rule): rule is VolumeExclusion =>
      rule &&
      typeof rule.volumeId === 'string' &&
      typeof rule.mountPath === 'string' &&
      typeof rule.relativePath === 'string' &&
      rule.relativePath.split('/').every((p: string) => p && p !== '.' && p !== '..'),
  );
}
