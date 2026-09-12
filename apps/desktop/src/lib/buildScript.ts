/**
 * Reading pnpm's "ignored build scripts" line out of a failed install job.
 *
 * A git-hosted plugin whose build script pnpm skipped leaves the install
 * half-done, and the fix is to approve that package in the profile's
 * `pnpm-workspace.yaml`. dsh names the packages; this only turns that line into
 * a starting point for the input, so the user does not retype a name that is
 * already on screen. It is never acted on by itself — the user confirms the
 * name, because pnpm's output format is pnpm's to change and a wrong guess here
 * would write an approval for a package nobody asked about.
 */

/** pnpm's line, e.g. `Ignored build scripts: esbuild, sharp.` */
const IGNORED_BUILD = /ignored build scripts?:\s*([^\n]+)/i

/** A first candidate package name from the job's stderr tail, or `''`. */
export function suggestBuildPackage(stderrTail: string | null): string {
  const match = stderrTail ? IGNORED_BUILD.exec(stderrTail) : null
  if (!match) return ''
  const first = match[1].split(/[,.]/)[0]?.trim() ?? ''
  // Only a plausible name; anything else (a sentence, an ANSI fragment) is left
  // for the user to type rather than put in the box as if it were the answer.
  return /^[@A-Za-z0-9._/-]+$/.test(first) ? first : ''
}
