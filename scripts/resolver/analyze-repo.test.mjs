// Unit tests for the pure resolver logic (no network — verify() is stubbed).
//   node --test scripts/resolver

import { test } from 'node:test'
import assert from 'node:assert/strict'
import {
  classify,
  ownerRepoOf,
  parsePackageJson,
  parsePyproject,
  parseSetupPy,
  decompileDockerfile,
  decide,
} from './analyze-repo.mjs'

// A verify() stub: names we claim exist on each registry.
const makeVerify = ({ npm = [], pypi = [] } = {}) => {
  const npmSet = new Set(npm)
  const pypiSet = new Set(pypi)
  return async (name, kind) => (kind === 'npm' ? npmSet.has(name) : pypiSet.has(name))
}

test('classify: real vs pseudo vs none', () => {
  assert.equal(classify({ command: 'npx', args: ['-y', '@x/y'] }), 'real')
  assert.equal(classify({ command: 'npx', args: ['-y', 'github:a/b'] }), 'pseudo')
  assert.equal(classify({ command: 'uvx', args: ['git+https://github.com/a/b'] }), 'pseudo')
  assert.equal(classify({ command: null, args: null }), 'none')
})

test('ownerRepoOf parses github urls', () => {
  assert.deepEqual(ownerRepoOf('https://github.com/foo/bar'), { owner: 'foo', repo: 'bar' })
  assert.deepEqual(ownerRepoOf('https://github.com/foo/bar.git'), { owner: 'foo', repo: 'bar' })
  assert.equal(ownerRepoOf('https://example.com/x'), null)
})

test('parsePackageJson: name/bin/workspaces', () => {
  const one = parsePackageJson(JSON.stringify({ name: '@s/pkg', bin: { '@s/pkg': './cli.js' } }))
  assert.deepEqual(one, {
    name: '@s/pkg',
    private: false,
    workspaces: false,
    binName: '@s/pkg',
    hasBin: true,
  })
  const strBin = parsePackageJson(JSON.stringify({ name: 'plain', bin: './server.js' }))
  assert.equal(strBin.binName, 'plain')
  const ws = parsePackageJson(JSON.stringify({ name: 'mono', private: true, workspaces: ['packages/*'] }))
  assert.equal(ws.workspaces, true)
  assert.equal(ws.private, true)
  assert.equal(parsePackageJson('{bad json'), null)
  assert.equal(parsePackageJson('{}'), null)
})

test('parsePyproject: name + scripts', () => {
  const t = `[project]\nname = "mcp-srv"\nversion = "0.1.0"\n\n[project.scripts]\nmcp-srv = "mcp_srv.main:main"\n\n[tool.uv]\npackage = true\n`
  const py = parsePyproject(t)
  assert.equal(py.projectName, 'mcp-srv')
  assert.deepEqual(py.scripts, ['mcp-srv'])
  assert.equal(parsePyproject('[tool.black]\nline-length = 88\n'), null)
})

test('parseSetupPy: name extraction', () => {
  assert.equal(parseSetupPy('from setuptools import setup\nsetup(name="mcp-git", version="0.1")'), 'mcp-git')
  assert.equal(parseSetupPy("name='other'"), 'other')
  assert.equal(parseSetupPy('# nothing'), null)
})

test('decompileDockerfile: node global install + cmd', () => {
  const d = decompileDockerfile(`
FROM node:20-alpine
RUN npm install -g @acme/mcp-server
CMD ["node", "dist/index.js"]
`)
  assert.equal(d.runtime, 'node')
  assert.deepEqual(d.npmPkgs, ['@acme/mcp-server'])
  assert.deepEqual(d.cmd, ['node', 'dist/index.js'])
})

test('decompileDockerfile: python pip install (joined continuation)', () => {
  const d = decompileDockerfile(`
FROM python:3.12-slim
RUN pip install --no-cache-dir \\
    mcp-server-git \\
    flask
`)
  assert.equal(d.runtime, 'python')
  assert.deepEqual(d.pipPkgs, ['mcp-server-git', 'flask'])
})

test('decompileDockerfile: multi-stage keeps final FROM runtime', () => {
  const d = decompileDockerfile(`
FROM node:20 AS build
RUN npm ci
FROM node:20-alpine
COPY --from=build /out /app
CMD ["node", "server.js"]
`)
  assert.equal(d.runtime, 'node')
})

test('decide: node published → canonical npx -y', async () => {
  const pkg = parsePackageJson(JSON.stringify({ name: '@acme/mcp', bin: { '@acme/mcp': 'x.js' } }))
  const r = await decide({
    owner: 'o',
    repo: 'r',
    hint: 'node',
    pkg,
    py: null,
    setupPy: null,
    req: false,
    docker: null,
    unsupported: null,
    verify: makeVerify({ npm: ['@acme/mcp'] }),
  })
  assert.equal(r.reason, null)
  assert.deepEqual(r.install, {
    runtime: 'node',
    method: 'npm',
    package: '@acme/mcp',
    launch: { command: 'npx', args: ['-y', '@acme/mcp'] },
  })
})

test('decide: node unpublished w/ bin → npx github:…', async () => {
  const pkg = parsePackageJson(JSON.stringify({ name: 'acme', bin: { acme: './cli.js' } }))
  const r = await decide({
    owner: 'o',
    repo: 'r',
    hint: 'node',
    pkg,
    py: null,
    setupPy: null,
    req: false,
    docker: null,
    unsupported: null,
    verify: makeVerify({ npm: [] }),
  })
  assert.equal(r.install.method, 'npm')
  assert.equal(r.install.package, 'github:o/r')
  assert.deepEqual(r.install.launch.args, ['-y', 'github:o/r'])
})

test('decide: node unpublished w/o bin → unresolved', async () => {
  const pkg = parsePackageJson(JSON.stringify({ name: 'lib', main: 'index.js' }))
  const r = await decide({
    owner: 'o',
    repo: 'r',
    hint: 'node',
    pkg,
    py: null,
    setupPy: null,
    req: false,
    docker: null,
    unsupported: null,
    verify: makeVerify({ npm: [] }),
  })
  assert.equal(r.install, null)
  assert.match(r.reason, /no-bin/)
})

test('decide: python published → uvx name', async () => {
  const py = parsePyproject('[project]\nname = "mcp-git"\n')
  const r = await decide({
    owner: 'o',
    repo: 'r',
    hint: 'python',
    pkg: null,
    py,
    setupPy: null,
    req: false,
    docker: null,
    unsupported: null,
    verify: makeVerify({ pypi: ['mcp-git'] }),
  })
  assert.deepEqual(r.install.launch, { command: 'uvx', args: ['mcp-git'] })
})

test('decide: python unpublished w/ scripts → uvx --from git+', async () => {
  const py = parsePyproject('[project]\nname = "srv"\n[project.scripts]\nsrv = "srv.main:main"\n')
  const r = await decide({
    owner: 'o',
    repo: 'r',
    hint: 'python',
    pkg: null,
    py,
    setupPy: null,
    req: false,
    docker: null,
    unsupported: null,
    verify: makeVerify({ pypi: [] }),
  })
  assert.equal(r.install.package, 'git+https://github.com/o/r')
  assert.deepEqual(r.install.launch.args, ['--from', 'git+https://github.com/o/r', 'srv'])
})

test('decide: python requirements-only → unresolved', async () => {
  const r = await decide({
    owner: 'o',
    repo: 'r',
    hint: 'python',
    pkg: null,
    py: null,
    setupPy: null,
    req: true,
    docker: null,
    unsupported: null,
    verify: makeVerify({}),
  })
  assert.equal(r.install, null)
  assert.match(r.reason, /requirements/)
})

test('decide: dockerfile python pip pkg published (no hint)', async () => {
  const docker = decompileDockerfile('FROM python:3.12\nRUN pip install mcp-server-git\n')
  const r = await decide({
    owner: 'o',
    repo: 'r',
    hint: null,
    pkg: null,
    py: null,
    setupPy: null,
    req: false,
    docker,
    unsupported: null,
    verify: makeVerify({ pypi: ['mcp-server-git'] }),
  })
  assert.equal(r.install.method, 'uv')
  assert.equal(r.install.package, 'mcp-server-git')
})

test('decide: go.mod → unsupported marker', async () => {
  const r = await decide({
    owner: 'o',
    repo: 'r',
    hint: null,
    pkg: null,
    py: null,
    setupPy: null,
    req: false,
    docker: null,
    unsupported: 'rust',
    verify: makeVerify({}),
  })
  assert.equal(r.install, null)
  assert.match(r.reason, /unsupported:rust/)
})

// Regression: a docker-only node repo that apt-installs a browser (the real
// Narasimhaponnada/mermaid-mcp Dockerfile shape) must NOT be resolved to the
// npm `chromium` package — chromium is an OS dep, not the MCP entry.
test('decompileDockerfile: apt/system installs never feed npmPkgs (mermaid-mcp)', () => {
  const d = decompileDockerfile(`
FROM node:18-slim AS builder
RUN cd server && npm ci --ignore-scripts && npm cache clean --force
FROM node:18-slim
RUN apt-get update && apt-get install -y \\
    chromium fonts-liberation fonts-noto-color-emoji \\
    libappindicator3-1 libasound2
RUN cd server && npm ci --omit=dev --ignore-scripts
CMD ["node", "dist/cli.js", "rest"]
`)
  assert.equal(d.runtime, 'node')
  assert.deepEqual(d.npmPkgs, []) // apt + lockfile-only npm ci → no runnable package
  assert.deepEqual(d.cmd, ['node', 'dist/cli.js', 'rest'])
})

test('decompileDockerfile: python image apt vs pip separation', () => {
  const d = decompileDockerfile(`
FROM python:3.12-slim
RUN apt-get install -y ffmpeg chromium
RUN pip install mcp-server-git flask
`)
  assert.equal(d.runtime, 'python')
  assert.deepEqual(d.pipPkgs, ['mcp-server-git', 'flask'])
})

test('decide: docker-only node repo (apt browser) is unresolved, never chromium', async () => {
  // pkg/py are null (no root manifest); even a docker carry-over that still lists
  // chromium must be rejected by the NON_SERVER_DEPS guard, never resolved to it.
  const r = await decide({
    owner: 'Narasimhaponnada',
    repo: 'mermaid-mcp',
    hint: 'node',
    pkg: null,
    py: null,
    setupPy: null,
    req: false,
    docker: { runtime: 'node', npmPkgs: ['chromium'], pipPkgs: [] },
    unsupported: null,
    verify: makeVerify({ npm: ['chromium'] }),
  })
  assert.equal(r.install, null) // unresolved, never a chromium manifest
  assert.match(r.reason, /no-node-fingerprint/)
})

test('decide: self-name colliding with an SDK (name "mcp") is not its own publish', async () => {
  // A repo whose package.json is literally named "mcp" is unpublished-as-its-own:
  // verifying "mcp" would hit the official Python/Node SDK package. It must fall
  // through to the bin/github run rather than resolve to the SDK.
  const pkg = parsePackageJson(JSON.stringify({ name: 'mcp', bin: 'cli.js' }))
  const r = await decide({
    owner: 'o',
    repo: 'r',
    hint: 'node',
    pkg,
    py: null,
    setupPy: null,
    req: false,
    docker: null,
    unsupported: null,
    verify: makeVerify({ npm: ['mcp'] }),
  })
  assert.ok(r.install) // fell through to github:o/r run (not the "mcp" SDK)
  assert.equal(r.install.method, 'npm')
  assert.equal(r.install.package, 'github:o/r')
})
