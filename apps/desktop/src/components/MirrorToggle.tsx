import { useAppStore } from '../stores/appStore'
import { useT } from '../lib/i18n'

/**
 * Plugin/skin GitHub mirror switch (gh-proxy). Default off — direct upstream.
 * Shown at the top-right of the Install Center surfaces so it can be flipped
 * before retrying a big skin/plugin that timed out on a throttled link. The
 * backend reads `settings.github_mirror` on every resolve, so the choice is
 * persisted and takes effect on the next clone/fetch.
 */
export function MirrorToggle() {
  const t = useT()
  const githubMirror = useAppStore((s) => s.settings?.githubMirror ?? false)
  const setGithubMirror = useAppStore((s) => s.setGithubMirror)

  return (
    <button
      type="button"
      role="switch"
      aria-checked={githubMirror}
      title={t('installCenter.mirrorHint')}
      onClick={() => void setGithubMirror(!githubMirror)}
      className={`flex h-7 items-center gap-1.5 rounded-md border px-2 text-[11px] transition-colors ${
        githubMirror
          ? 'border-blue-500/40 bg-blue-500/10 text-blue-200 hover:border-blue-500/70'
          : 'border-zinc-800 bg-zinc-900/40 text-zinc-400 hover:border-zinc-600 hover:text-zinc-200'
      }`}
    >
      <span
        className={`h-1.5 w-1.5 rounded-full ${
          githubMirror ? 'bg-blue-400 shadow-[0_0_6px_rgba(96,165,250,0.8)]' : 'bg-zinc-600'
        }`}
      />
      {githubMirror ? t('installCenter.mirrorOn') : t('installCenter.mirrorOff')}
    </button>
  )
}
