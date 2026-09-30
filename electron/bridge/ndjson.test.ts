import { describe, expect, test } from 'bun:test'
import { MAX_LINE_BYTES, createLineDecoder } from './ndjson'

describe('bounded NDJSON line decoder', () => {
  test('returns every line in a multi-line chunk', () => {
    const decoder = createLineDecoder()
    expect(decoder.push('{"a":1}\n{"a":2}\n')).toEqual(['{"a":1}', '{"a":2}'])
    expect(decoder.oversized).toBe(0)
  })

  test('keeps a partial line across chunks and tolerates CRLF', () => {
    const decoder = createLineDecoder()
    expect(decoder.push('{"a"')).toEqual([])
    expect(decoder.push(':1}\r\n{"b"')).toEqual(['{"a":1}'])
    expect(decoder.push(':2}\n')).toEqual(['{"b":2}'])
  })

  test('flushes the trailing unterminated line on end', () => {
    const decoder = createLineDecoder()
    expect(decoder.push('{"tail":true}')).toEqual([])
    expect(decoder.end()).toEqual(['{"tail":true}'])
    expect(decoder.end()).toEqual([])
  })

  test('drops a terminated oversized line and keeps framing intact', () => {
    const decoder = createLineDecoder(16)
    expect(decoder.push(`${'x'.repeat(64)}\n{"ok":true}\n`)).toEqual(['{"ok":true}'])
    expect(decoder.oversized).toBe(1)
  })

  test('drops an unterminated oversized line instead of buffering it', () => {
    const decoder = createLineDecoder(16)
    expect(decoder.push('y'.repeat(200))).toEqual([])
    expect(decoder.oversized).toBe(1)
    // The decoder is still aligned: the next newline ends the discarded line.
    expect(decoder.push('\n{"ok":true}\n')).toEqual(['{"ok":true}'])
    expect(decoder.end()).toEqual([])
  })

  test('accepts a line exactly at the limit', () => {
    const decoder = createLineDecoder(8)
    expect(decoder.push(`${'z'.repeat(8)}\n`)).toHaveLength(1)
    expect(decoder.oversized).toBe(0)
  })

  test('uses the Rust bridge line limit by default', () => {
    expect(MAX_LINE_BYTES).toBe(64 * 1024)
  })
})
