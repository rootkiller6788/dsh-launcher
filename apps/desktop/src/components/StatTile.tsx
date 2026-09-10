import type { LucideIcon } from 'lucide-react'

export function StatTile({
  icon: Icon,
  label,
  value,
}: {
  icon: LucideIcon
  label: string
  value: string | number
}) {
  return (
    <div className="rounded-lg border border-zinc-800/70 bg-zinc-950/25 px-4 py-3">
      <div className="flex items-center gap-2 text-[11px] text-zinc-500">
        <Icon className="h-3.5 w-3.5" strokeWidth={1.75} />
        <span className="truncate">{label}</span>
      </div>
      <div className="mt-1 truncate text-xl font-semibold tabular-nums text-zinc-100">{value}</div>
    </div>
  )
}
