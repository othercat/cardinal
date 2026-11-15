// Format bytes into KB with one decimal place (legacy function)
export function formatKB(bytes: number | null | undefined): string | null {
  if (bytes == null || !Number.isFinite(bytes)) return null;
  const kb = bytes / 1024;
  return `${kb.toFixed(kb < 10 ? 1 : 0)} KB`;
}

const diskUnits = ['B', 'KB', 'MB', 'GB', 'TB', 'PB', 'EB'];
const diskNumber = new Intl.NumberFormat(undefined, { maximumFractionDigits: 2 });

// Finder uses decimal storage units. A fully sparse file can legitimately use zero bytes.
export function formatDiskUsage(bytes: number | null | undefined): string | null {
  if (bytes == null || !Number.isFinite(bytes) || bytes < 0) return null;
  let unit = 0;
  while (bytes >= 1000 && unit < diskUnits.length - 1) {
    bytes /= 1000;
    unit++;
  }
  return `${diskNumber.format(bytes)} ${diskUnits[unit]}`;
}

// Format timestamp (in seconds) as YYYY-MM-DD HH:mm:ss
export function formatTimestamp(timestampSec: number | null | undefined): string | null {
  if (timestampSec == null || !Number.isFinite(timestampSec)) return null;
  const date = new Date(timestampSec * 1000);

  const year = date.getFullYear();
  const month = String(date.getMonth() + 1).padStart(2, '0');
  const day = String(date.getDate()).padStart(2, '0');
  const hours = String(date.getHours()).padStart(2, '0');
  const minutes = String(date.getMinutes()).padStart(2, '0');
  const seconds = String(date.getSeconds()).padStart(2, '0');

  return `${year}-${month}-${day} ${hours}:${minutes}:${seconds}`;
}
