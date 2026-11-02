// Tauri runs hooks from desktop/. Resolve everything from this file so callers
// from the repository root and paths containing spaces work as well.
import { spawnSync } from 'node:child_process'
import { copyFileSync, mkdirSync } from 'node:fs'
import { dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

export const repoRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..')

function run(command, args, cwd) {
  // npm is a .cmd shim on Windows. No user input is interpolated into commands.
  const result = spawnSync(command, args, { cwd, stdio: 'inherit', shell: process.platform === 'win32' })
  if (result.error) throw result.error
  if (result.status !== 0) throw new Error(`${command} failed (${result.signal ?? result.status})`)
}

export function prepareDesktop(mode, { root = repoRoot, execute = run, platform = process.platform } = {}) {
  if (!['build', 'dev'].includes(mode)) throw new Error('Usage: node scripts/desktop.mjs build|dev')
  execute('npm', ['--prefix', 'ui', 'ci'], root)
  execute('npm', ['--prefix', 'ui', 'run', 'build'], root)
  // Use a known target directory even when the caller has CARGO_TARGET_DIR set.
  // The UI must exist before Rust embeds it; only then replace the staged runtime.
  execute('cargo', ['build', '--locked', '--bin', 'misaka-studiod', '--target-dir', 'target', ...(mode === 'build' ? ['--release'] : [])], root)
  const name = platform === 'win32' ? 'misaka-studiod.exe' : 'misaka-studiod'
  const resources = join(root, 'desktop', 'src-tauri', 'resources')
  mkdirSync(resources, { recursive: true })
  copyFileSync(join(root, 'target', mode === 'build' ? 'release' : 'debug', name), join(resources, name))
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    const mode = process.argv[2]
    prepareDesktop(mode)
    if (mode === 'dev') run('npm', ['--prefix', 'ui', 'run', 'dev'], repoRoot)
  } catch (error) {
    console.error(error.message)
    process.exitCode = 1
  }
}
