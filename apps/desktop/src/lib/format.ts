/** Compact token count: `1.2K` / `3.4M`, or a grouped integer below a thousand. */
export function fmtTokens(n: number) {
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`
  if (n >= 1_000) return `${(n / 1_000).toFixed(1)}K`
  return n.toLocaleString()
}

/**
 * `1h 2m` / `3m 4s` / `5s` between two unix-second stamps. A missing end
 * defaults to now, so a still-running span keeps ticking.
 */
export function formatDuration(start?: number | null, end?: number | null) {
  if (!start) return '-'
  const stop = end ?? Math.floor(Date.now() / 1000)
  const s = Math.max(0, stop - start)
  const h = Math.floor(s / 3600)
  const m = Math.floor((s % 3600) / 60)
  const sec = s % 60
  return h > 0 ? `${h}h ${m}m` : m > 0 ? `${m}m ${sec}s` : `${sec}s`
}

/** The same shape as {@link formatDuration}, for an already-computed span. */
export function formatSeconds(seconds: number | null) {
  if (seconds == null) return '-'
  const h = Math.floor(seconds / 3600)
  const m = Math.floor((seconds % 3600) / 60)
  const sec = seconds % 60
  return h > 0 ? `${h}h ${m}m` : m > 0 ? `${m}m ${sec}s` : `${sec}s`
}
