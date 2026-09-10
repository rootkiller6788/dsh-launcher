// Pushes the current branch to both hosting accounts, each under its own commit
// identity, in one command.
//
// A commit carries exactly ONE author email, and that email is fixed when the
// commit is made — not when it is pushed. So "same commits, credited to a
// different account on each platform" is impossible for a single SHA: pushing
// one history to two hosts gives both hosts the same author. The only way to get
// attribution on both accounts separately is to give each host its own history.
//
// So this script builds one mirror branch per account — identical tree, message
// and dates, author + committer rewritten to that account — and pushes each
// mirror to its own remote. The two platforms end up with divergent SHAs; that
// is inherent to the approach, not a bug.
//
// The rewrite is deterministic (`filter-branch` preserves the author/committer
// dates), so a mirror's tip only moves when the source branch actually gains
// commits. Every run after the first is a fast-forward.
//
//   node scripts/push-both.mjs                  # push main to both accounts
//   node scripts/push-both.mjs --branch dev     # some other branch
//   node scripts/push-both.mjs --dry-run        # build the mirrors, push nothing
//
// The first run replaces whatever each remote currently has on that branch, so
// it is a one-time history rewrite on both hosts.

import { execFileSync } from 'node:child_process'

// The two accounts. `email` must be an address verified on THAT account, or the
// host will not link the commit to it. The two emails are expected to differ —
// that is the point: one host credits the GitHub account, the other the Gitee
// account.
const ACCOUNTS = [
  {
    label: 'github',
    remote: 'git@github.com:rootkiller6788/dsh-launcher.git',
    mirror: 'mirror-github',
    name: 'rootkiller6788',
    email: 'c8688rickowens@outlook.com',
  },
  {
    label: 'gitee',
    remote: 'https://gitee.com/rootkiller6788/dsh-launcher',
    mirror: 'mirror-gitee',
    name: 'rootkiller6788',
    email: '17553215+rootkiller6788@user.noreply.gitee.com',
  },
]

const DEFAULT_BRANCH = 'main'
const PLACEHOLDER = /REPLACE-ME|users\.noreply\.github\.com$/

const USAGE = `usage: node scripts/push-both.mjs [--branch <name>] [--dry-run]`

function fail(message) {
  console.error(`push-both: ${message}`)
  process.exit(1)
}

/** `git` with the output captured; trims the trailing newline every caller ignores. */
function git(args, opts = {}) {
  return execFileSync('git', args, { encoding: 'utf8', ...opts }).trim()
}

/** `git` with the terminal handed over, for the steps worth watching live. */
function gitLive(args, opts = {}) {
  execFileSync('git', args, { stdio: 'inherit', ...opts })
}

function parseArgs(argv) {
  const args = { branch: DEFAULT_BRANCH, dryRun: false }
  for (let i = 0; i < argv.length; i += 1) {
    const flag = argv[i]
    if (flag === '--dry-run') args.dryRun = true
    else if (flag === '--branch') args.branch = argv[++i] ?? fail('--branch needs a name')
    else fail(`unknown option ${flag}\n${USAGE}`)
  }
  return args
}

/**
 * `filter-branch` refuses to run over edits to tracked files — it would rewrite
 * the branch out from under them. Untracked files are none of its business, and
 * the new script sitting in the tree is the common case, so they are excluded
 * from the check.
 */
function assertCleanTree() {
  const dirty = git(['status', '--porcelain', '--untracked-files=no'])
  if (dirty) fail('tracked files have uncommitted changes — commit or stash them first')
}

function envFilter(account) {
  // The filter is eval'd as shell, so keep the identity to a charset that cannot
  // close the quoting.
  const safe = /^[A-Za-z0-9 ._+@-]+$/
  if (!safe.test(account.name) || !safe.test(account.email)) {
    fail(`${account.label}: name/email contains characters the env-filter cannot quote safely`)
  }
  return [
    `export GIT_AUTHOR_NAME='${account.name}'`,
    `GIT_AUTHOR_EMAIL='${account.email}'`,
    `GIT_COMMITTER_NAME='${account.name}'`,
    `GIT_COMMITTER_EMAIL='${account.email}'`,
  ].join(' ')
}

/** Rebuild `account.mirror` from `source` under the account's identity. */
function buildMirror(account, source) {
  git(['branch', '--force', account.mirror, source])
  gitLive(
    ['filter-branch', '--force', '--env-filter', envFilter(account), '--', account.mirror],
    // filter-branch prints a deprecation notice on every run; the header above
    // is the explanation it is asking for.
    { env: { ...process.env, FILTER_BRANCH_SQUELCH_WARNING: '1' }, stdio: ['ignore', 'ignore', 'inherit'] },
  )
  // filter-branch parks the pre-rewrite tip in refs/original. It is never the
  // branch we want back, and leaving it makes the next run's backup ambiguous.
  try {
    git(['update-ref', '-d', `refs/original/refs/heads/${account.mirror}`])
  } catch {
    /* nothing parked (--force on an already-clean mirror) */
  }
  return git(['rev-parse', account.mirror])
}

function pushMirror(account, branch) {
  const dest = `refs/heads/${branch}`
  // Lease on the exact remote value we just read, so a push that raced with
  // someone else's is rejected instead of silently overwritten.
  let lease = null
  try {
    const probe = git(['ls-remote', account.remote, dest], {
      env: { ...process.env, GIT_TERMINAL_PROMPT: '0' },
    })
    lease = probe.split(/\s+/)[0] || null
  } catch {
    /* unreachable or auth-prompting: fall through to a plain force push */
  }
  const args = ['push']
  if (lease) args.push(`--force-with-lease=${dest}:${lease}`)
  else args.push('--force')
  args.push(account.remote, `${account.mirror}:${branch}`)
  gitLive(args)
}

function main() {
  const { branch, dryRun } = parseArgs(process.argv.slice(2))

  for (const account of ACCOUNTS) {
    if (PLACEHOLDER.test(account.email)) {
      fail(
        `${account.label} still has a placeholder email — put the address verified on that ` +
          `account into ACCOUNTS at the top of this script`,
      )
    }
  }

  assertCleanTree()
  try {
    git(['rev-parse', '--verify', `refs/heads/${branch}`])
  } catch {
    fail(`no local branch ${branch} — pass --branch <name>`)
  }

  const source = git(['rev-parse', branch])
  console.log(`source: ${branch} @ ${source.slice(0, 7)}${dryRun ? '  (dry run)' : ''}`)

  for (const account of ACCOUNTS) {
    const tip = buildMirror(account, branch)
    console.log(`\n${account.label}: ${account.mirror} @ ${tip.slice(0, 7)}  <${account.email}>`)
    if (dryRun) continue
    pushMirror(account, branch)
  }

  if (dryRun) console.log('\ndry run — nothing pushed')
}

main()
