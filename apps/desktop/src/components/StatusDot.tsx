const COLORS: Record<string, string> = {
  stopped: 'bg-zinc-500',
  starting: 'bg-amber-400 animate-pulse',
  running: 'bg-emerald-400 shadow-[0_0_8px] shadow-emerald-400/60',
  degraded: 'bg-orange-400',
  crashed: 'bg-red-500',
}

export function StatusDot({ status }: { status: string }) {
  return (
    <span
      className={`inline-block h-2.5 w-2.5 rounded-full ${COLORS[status] ?? 'bg-zinc-500'}`}
    />
  )
}
