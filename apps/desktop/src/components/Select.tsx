import { useEffect, useRef, useState } from 'react'
import { ChevronDown } from 'lucide-react'

export interface SelectOption<T extends string> {
  label: string
  value: T
}

/**
 * Theme-aware dropdown, matching the Market "全部分类" filter's custom style.
 * Native `<select>` popups are drawn by WebView2/OS and ignore the in-page
 * light/dark palette; a DOM panel uses the same zinc tokens as the rest of the
 * UI, so it follows `html[data-theme="light"]` like every other surface.
 */
export function Select<T extends string>({
  value,
  onChange,
  options,
  label,
  triggerClassName,
  panelClassName,
}: {
  value: T
  onChange: (value: T) => void
  options: SelectOption<T>[]
  /** Leading small label shown before the selection (e.g. Activity filters). */
  label?: string
  triggerClassName?: string
  panelClassName?: string
}) {
  const [open, setOpen] = useState(false)
  const rootRef = useRef<HTMLDivElement>(null)

  // Close on Escape and on clicks outside the control (the panel sits in an
  // absolutely-positioned overlay — a `fixed inset-0` sibling swallows the
  // outside clicks, and this handler covers the rest).
  useEffect(() => {
    if (!open) return
    const onKey = (event: KeyboardEvent) => {
      if (event.key === 'Escape') setOpen(false)
    }
    const onPointer = (event: PointerEvent) => {
      if (!rootRef.current?.contains(event.target as Node)) setOpen(false)
    }
    document.addEventListener('keydown', onKey)
    document.addEventListener('pointerdown', onPointer)
    return () => {
      document.removeEventListener('keydown', onKey)
      document.removeEventListener('pointerdown', onPointer)
    }
  }, [open])

  const selected = options.find((option) => option.value === value)?.label ?? value
  const pick = (next: T) => {
    onChange(next)
    setOpen(false)
  }

  return (
    <div ref={rootRef} className="relative min-w-0">
      <button
        type="button"
        onClick={() => setOpen((o) => !o)}
        aria-haspopup="listbox"
        aria-expanded={open}
        className={`flex w-full items-center justify-between gap-2 rounded-lg border border-zinc-800 bg-zinc-950/35 px-3 text-sm text-zinc-200 outline-none hover:border-zinc-700 focus:border-blue-500 ${triggerClassName ?? ''}`}
      >
        <span className="flex min-w-0 items-baseline gap-1.5">
          {label && <span className="shrink-0 text-xs text-zinc-500">{label}</span>}
          <span className="truncate">{selected}</span>
        </span>
        <ChevronDown
          className={`h-4 w-4 shrink-0 text-zinc-500 transition-transform ${open ? 'rotate-180' : ''}`}
          strokeWidth={1.75}
        />
      </button>
      {open && (
        <div
          role="listbox"
          className={`no-scrollbar absolute left-0 z-50 mt-1 max-h-64 min-w-full max-w-xs overflow-y-auto rounded-lg border border-zinc-700 bg-zinc-900 p-1 shadow-xl ${panelClassName ?? ''}`}
        >
          {options.map((option) => (
            <button
              key={option.value}
              type="button"
              role="option"
              aria-selected={option.value === value}
              onClick={() => pick(option.value)}
              title={option.label}
              className={`block w-full truncate rounded px-3 py-1 text-left text-sm ${
                option.value === value
                  ? 'bg-blue-600/20 text-blue-300'
                  : 'text-zinc-300 hover:bg-zinc-800'
              }`}
            >
              {option.label}
            </button>
          ))}
        </div>
      )}
    </div>
  )
}
