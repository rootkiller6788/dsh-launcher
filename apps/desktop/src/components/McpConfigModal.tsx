import { useEffect, useState } from 'react'
import { CheckCircle2, Loader2, Plus, Trash2, X } from 'lucide-react'
import { ipc } from '../lib/ipc'
import { useT } from '../lib/i18n'
import type { McpConfigVar, McpEnvRequirement } from '../lib/types'

/** One fillable/configurable variable in the form. Declared rows (`missing`)
 * are read-only on the key; custom rows let the user type any key. Values are
 * only ever sent to the backend — never loaded back out (secrets stay in the OS
 * credential store; the UI only knows a key is set). */
interface VarRow {
  key: string
  secret: boolean
  label?: string | null
  custom?: boolean
  value: string
}

export function McpConfigModal({
  instanceId,
  serverId,
  serverName,
  missing,
  runtimeError,
  onClose,
  onSaved,
}: {
  instanceId: string
  serverId: string
  serverName: string
  /** Declared required keys still unset (from the Library snapshot). */
  missing: McpEnvRequirement[]
  /** The probe-degraded reason, when an undeclared server self-reported a gap. */
  runtimeError: string | null
  onClose: () => void
  onSaved: () => void
}) {
  const t = useT()
  const [rows, setRows] = useState<VarRow[]>(() =>
    missing.map((m) => ({ key: m.key, secret: m.secret, label: m.label ?? null, value: '' })),
  )
  const [configured, setConfigured] = useState<McpConfigVar[]>([])
  const [saving, setSaving] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [saved, setSaved] = useState(false)

  useEffect(() => {
    let live = true
    ipc
      .mcpConfigGet(instanceId, serverId)
      .then((list) => {
        if (live) setConfigured(list)
      })
      .catch(() => {
        /* stale/backend hiccup — configured list stays empty, form still works */
      })
    return () => {
      live = false
    }
  }, [instanceId, serverId])

  // Keys already configured that aren't also in the fill form (declared rows).
  const chips = configured.filter((c) => !missing.some((m) => m.key === c.key))

  const patch = (i: number, fn: (r: VarRow) => VarRow) =>
    setRows((rs) => rs.map((r, j) => (j === i ? fn(r) : r)))
  const addRow = () =>
    setRows((rs) => [...rs, { key: '', secret: true, custom: true, value: '' }])
  const dropRow = (i: number) => setRows((rs) => rs.filter((_, j) => j !== i))

  const doSave = async () => {
    setSaving(true)
    setError(null)
    setSaved(false)
    const entries = rows
      .filter((r) => r.key.trim() && r.value.length > 0)
      .map((r) => ({ key: r.key.trim(), secret: r.secret, value: r.value }))
    try {
      const list = await ipc.mcpConfigSave(instanceId, serverId, entries)
      setConfigured(list)
      setSaved(true)
      setSaving(false)
      onSaved() // parent closes + refreshes the snapshot so the row hint clears
    } catch (e) {
      setError(t('mcp.config.error') + String(e))
      setSaving(false)
    }
  }

  const doRemove = async (key: string) => {
    setError(null)
    try {
      setConfigured(await ipc.mcpConfigRemove(instanceId, serverId, key))
    } catch (e) {
      setError(t('mcp.config.error') + String(e))
    }
  }

  return (
    <div
      className="fixed inset-0 z-[70] flex items-center justify-center bg-black/70 p-6"
      onClick={onClose}
    >
      <div
        className="flex max-h-[85vh] w-full max-w-lg flex-col rounded-xl border border-zinc-700 bg-zinc-900 shadow-2xl"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="flex items-start justify-between gap-4 border-b border-zinc-800 px-5 py-4">
          <div className="min-w-0">
            <h2 className="text-lg font-semibold text-zinc-50">{t('mcp.config.title')}</h2>
            <p className="mt-0.5 truncate font-mono text-xs text-zinc-500" title={serverName}>
              {serverName}
            </p>
          </div>
          <button
            onClick={onClose}
            className="rounded-lg p-1.5 text-zinc-400 hover:bg-zinc-800 hover:text-zinc-100"
          >
            <X className="h-4 w-4" strokeWidth={1.75} />
          </button>
        </div>

        <div className="min-h-0 flex-1 space-y-4 overflow-y-auto px-5 py-4">
          {runtimeError && (
            <p className="rounded-lg border border-amber-500/25 bg-amber-500/10 px-3 py-2 text-xs leading-5 text-amber-300">
              <span className="font-medium">{t('mcp.config.runtimeHint')}</span> {runtimeError}
            </p>
          )}

          {rows.length === 0 && chips.length === 0 && (
            <p className="text-xs text-zinc-500">{t('mcp.config.empty')}</p>
          )}

          {rows.length > 0 && (
            <div className="space-y-2">
              <div className="text-[11px] font-medium uppercase tracking-wide text-zinc-500">
                {missing.length > 0 ? t('mcp.config.toFill') : t('mcp.config.customTitle')}
              </div>
              {rows.map((row, i) => (
                <div key={i} className="rounded-lg border border-zinc-800 bg-zinc-950/40 p-2.5">
                  <div className="flex items-center gap-2">
                    {row.custom ? (
                      <input
                        value={row.key}
                        onChange={(e) => patch(i, (r) => ({ ...r, key: e.target.value }))}
                        placeholder="KEY_NAME"
                        spellCheck={false}
                        className="min-w-0 flex-1 rounded-md border border-zinc-700 bg-zinc-900 px-2 py-1 font-mono text-xs text-zinc-200 placeholder-zinc-600 focus:border-cyan-500/60 focus:outline-none"
                      />
                    ) : (
                      <div className="min-w-0 flex-1">
                        <div className="truncate font-mono text-xs font-semibold text-zinc-200">
                          {row.secret ? '🔑 ' : ''}
                          {row.key}
                        </div>
                        {row.label && (
                          <div className="truncate text-[10px] text-zinc-500">{row.label}</div>
                        )}
                      </div>
                    )}
                    {row.custom && (
                      <label className="flex shrink-0 items-center gap-1.5 text-[11px] text-zinc-500">
                        <input
                          type="checkbox"
                          checked={row.secret}
                          onChange={(e) => patch(i, (r) => ({ ...r, secret: e.target.checked }))}
                          className="h-3.5 w-3.5 accent-cyan-500"
                        />
                        {t('mcp.config.secretValue')}
                      </label>
                    )}
                    <button
                      type="button"
                      onClick={() => dropRow(i)}
                      title={t('mcp.config.remove')}
                      className="rounded p-1 text-zinc-500 hover:bg-zinc-800 hover:text-red-300"
                    >
                      <Trash2 className="h-3.5 w-3.5" strokeWidth={1.75} />
                    </button>
                  </div>
                  <input
                    type={row.secret ? 'password' : 'text'}
                    value={row.value}
                    onChange={(e) => patch(i, (r) => ({ ...r, value: e.target.value }))}
                    placeholder={row.secret ? '••••••••' : 'value'}
                    spellCheck={false}
                    autoComplete="off"
                    className="mt-2 w-full rounded-md border border-zinc-700 bg-zinc-900 px-2 py-1.5 font-mono text-xs text-zinc-200 placeholder-zinc-600 focus:border-cyan-500/60 focus:outline-none"
                  />
                  <div className="mt-1 text-[10px] text-zinc-600">{t('mcp.config.blankKeeps')}</div>
                </div>
              ))}
              <button
                type="button"
                onClick={addRow}
                className="flex w-full items-center justify-center gap-1.5 rounded-lg border border-dashed border-zinc-700 py-2 text-xs font-medium text-zinc-400 hover:border-zinc-500 hover:text-zinc-200"
              >
                <Plus className="h-3.5 w-3.5" strokeWidth={1.75} />
                {t('mcp.config.addVar')}
              </button>
            </div>
          )}

          {chips.length > 0 && (
            <div>
              <div className="text-[11px] font-medium uppercase tracking-wide text-zinc-500">
                {t('mcp.config.configured')}
              </div>
              <div className="mt-1.5 flex flex-wrap gap-1.5">
                {chips.map((c) => (
                  <span
                    key={c.key}
                    className="inline-flex items-center gap-1.5 rounded-full border border-emerald-500/25 bg-emerald-500/10 px-2 py-1 font-mono text-[11px] text-emerald-300"
                  >
                    {c.secret ? '🔑 ' : ''}
                    {c.key}
                    <button
                      type="button"
                      onClick={() => void doRemove(c.key)}
                      title={t('mcp.config.remove')}
                      className="rounded-full p-0.5 text-emerald-400/70 hover:bg-emerald-500/20 hover:text-emerald-200"
                    >
                      <X className="h-3 w-3" strokeWidth={2} />
                    </button>
                  </span>
                ))}
              </div>
            </div>
          )}
        </div>

        <div className="flex items-center justify-between gap-3 border-t border-zinc-800 px-5 py-3">
          {error ? (
            <p className="min-w-0 flex-1 truncate text-xs text-red-400">{error}</p>
          ) : saved ? (
            <p className="flex items-center gap-1.5 text-xs text-emerald-400">
              <CheckCircle2 className="h-3.5 w-3.5" strokeWidth={1.75} />
              {t('mcp.config.saved')}
            </p>
          ) : (
            <p className="flex-1 text-[11px] leading-4 text-zinc-600">
              {t('mcp.config.vaultHint')}
            </p>
          )}
          <div className="flex shrink-0 items-center gap-2">
            <button
              type="button"
              onClick={onClose}
              className="rounded-lg border border-zinc-700 px-3 py-1.5 text-xs font-medium text-zinc-400 hover:border-zinc-500 hover:text-zinc-200"
            >
              {t('mcp.config.cancel')}
            </button>
            <button
              type="button"
              onClick={() => void doSave()}
              disabled={saving || rows.length === 0}
              className="flex items-center gap-1.5 rounded-lg bg-cyan-500 px-3 py-1.5 text-xs font-semibold text-zinc-950 hover:bg-cyan-400 disabled:cursor-not-allowed disabled:opacity-45"
            >
              {saving && <Loader2 className="h-3.5 w-3.5 animate-spin" strokeWidth={2} />}
              {t('mcp.config.save')}
            </button>
          </div>
        </div>
      </div>
    </div>
  )
}
