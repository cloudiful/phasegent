// Print the FNV-1a 64-bit content hash of the `frontend/dist` tree.
//
// Mirrors the byte-for-byte algorithm in `build.rs` (`frontend_dist_hash`)
// so the operator can compare this value against the embedded hash the
// running binary reports on the GUI Status page. If the two differ, the
// installed binary embeds a different bundle than the current `frontend/dist`.

import { existsSync, readdirSync, readFileSync } from 'node:fs'
import { join, resolve, sep } from 'node:path'
import { fileURLToPath } from 'node:url'

const defaultDistPath = fileURLToPath(new URL('../frontend/dist/', import.meta.url))
const distPath = process.argv[2] ? resolve(process.argv[2]) : defaultDistPath

const FNV_OFFSET = 0xcbf29ce484222325n
const FNV_PRIME = 0x00000100000001b3n

function fnvStep(hash, byte) {
  return ((hash ^ BigInt(byte)) * FNV_PRIME) % 0x10000000000000000n
}

function collectFiles(root, prefix) {
  const out = []
  for (const entry of readdirSync(join(root, prefix), { withFileTypes: true })) {
    const rel = prefix ? `${prefix}${sep}${entry.name}` : entry.name
    if (entry.isDirectory()) {
      out.push(...collectFiles(root, rel))
    }
    else {
      out.push(rel)
    }
  }
  return out
}

if (!existsSync(distPath)) {
  console.error('dist-hash: FAIL: frontend/dist is missing; run the Vite build first.')
  process.exit(1)
}

const files = collectFiles(distPath, '').map(path => path.split(sep).join('/')).sort()
if (files.length === 0) {
  console.error('dist-hash: FAIL: frontend/dist is empty.')
  process.exit(1)
}

let hash = FNV_OFFSET
for (const relative of files) {
  for (const byte of Buffer.from(relative, 'utf8')) hash = fnvStep(hash, byte)
  hash = fnvStep(hash, 0x00)
  const bytes = readFileSync(join(distPath, relative))
  for (const byte of bytes) hash = fnvStep(hash, byte)
  hash = fnvStep(hash, 0x00)
}

const hex = hash.toString(16).padStart(16, '0')
console.log(hex)
