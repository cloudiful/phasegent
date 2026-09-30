// Contract test: the Electron method table must stay identical to the Rust
// bridge allowlist in `src/desktop_bridge.rs`. Any drift (a renamed wire
// method, a changed parameter field, a bumped protocol version) fails here
// instead of failing at runtime against a mismatched packaged backend.

import { readFileSync } from 'node:fs'
import { describe, expect, test } from 'bun:test'
import {
  DESKTOP_CHANNELS,
  DESKTOP_METHODS,
  DESKTOP_METHOD_NAMES,
  DESKTOP_PROTOCOL_VERSION,
  MAX_REQUEST_BYTES,
  channelFor,
  isDesktopMethod,
  type DesktopMethod,
} from './methods'

const rust = readFileSync(new URL('../../src/desktop_bridge.rs', import.meta.url), 'utf8')

function rustMethodTable(): Array<{ variant: string, wire: string }> {
  const block = rust.slice(rust.indexOf('const METHODS'), rust.indexOf('impl Method'))
  return [...block.matchAll(/\(Method::(\w+), "([a-z_]+)"\)/g)].map(match => ({
    variant: match[1]!,
    wire: match[2]!,
  }))
}

function rustParamField(variant: string): string | null {
  const arm = rust.indexOf(`Method::${variant} =>`)
  if (arm < 0) throw new Error(`dispatch arm for ${variant} not found`)
  const next = rust.indexOf('Method::', arm + 1)
  const body = rust.slice(arm, next < 0 ? rust.length : next)
  const decoded = body.match(/decode\(method, params, "(\w+)"\)/)
  return decoded ? decoded[1]! : null
}

describe('desktop method table parity with src/desktop_bridge.rs', () => {
  test('protocol version matches the Rust bridge', () => {
    const version = rust.match(/pub\(crate\) const PROTOCOL_VERSION: u32 = (\d+);/)
    expect(version?.[1]).toBe(String(DESKTOP_PROTOCOL_VERSION))
  })

  test('wire method names and order match the Rust allowlist', () => {
    expect(rustMethodTable().map(entry => entry.wire)).toEqual([...DESKTOP_METHOD_NAMES])
  })

  test('every method decodes the same parameter field as the Rust dispatch', () => {
    const expected: Record<string, string | null> = {}
    const actual: Record<string, string | null> = {}
    for (const { variant, wire } of rustMethodTable()) {
      expected[wire] = rustParamField(variant)
      actual[wire] = DESKTOP_METHODS[wire as DesktopMethod].param
    }
    expect(actual).toEqual(expected)
  })

  test('the renderer payload bound stays below the Rust line bound', () => {
    const limit = rust.match(/const MAX_LINE_BYTES: usize = (\d+) \* (\d+);/)
    expect(limit).not.toBeNull()
    const rustLimit = Number(limit![1]) * Number(limit![2])
    expect(MAX_REQUEST_BYTES).toBeGreaterThan(0)
    expect(MAX_REQUEST_BYTES).toBeLessThan(rustLimit)
  })
})

describe('allowlisted method set', () => {
  test('every method has a unique, prefixed channel', () => {
    expect(DESKTOP_CHANNELS).toHaveLength(DESKTOP_METHOD_NAMES.length)
    expect(new Set(DESKTOP_CHANNELS).size).toBe(DESKTOP_CHANNELS.length)
    for (const method of DESKTOP_METHOD_NAMES) {
      expect(channelFor(method)).toBe(`phasegent:${method}`)
    }
  })

  test('CLI groups and arbitrary names are not desktop methods', () => {
    for (const name of ['get_issue', 'issue', 'gui', 'desktop-bridge', 'comment', 'admin', '']) {
      expect(isDesktopMethod(name)).toBe(false)
    }
    expect(() => channelFor('get_issue' as never)).toThrow()
  })

  test('methods without parameters keep a null parameter field', () => {
    for (const method of ['get_app_metadata', 'get_config_snapshot', 'get_branch_context'] as const) {
      expect(DESKTOP_METHODS[method].param).toBeNull()
    }
  })
})
