import { describe, expect, test } from 'bun:test'
import { readFileSync } from 'node:fs'
import { DEFAULT_BACKEND_ARGS, spawnBridgeChannel } from './child-transport'
import { createFakeChild, fakeSpawn, flush } from './fake-child'

function channelFor(child: ReturnType<typeof createFakeChild>, options: { shutdownGraceMs?: number } = {}) {
  const diagnostics: string[] = []
  const { spawn, calls } = fakeSpawn(child)
  const channel = spawnBridgeChannel({
    binary: '/opt/phasegent-backend',
    spawn,
    onDiagnostic: message => diagnostics.push(message),
    ...options,
  })
  return { channel, calls, diagnostics }
}

describe('backend transport', () => {
  test('spawns the companion with the hidden stdio mode', () => {
    const child = createFakeChild()
    const { calls } = channelFor(child)
    expect(calls).toEqual([{ command: '/opt/phasegent-backend', args: [...DEFAULT_BACKEND_ARGS] }])
  })

  test('keeps the stdio mode token identical to the Rust dispatch token', () => {
    const rust = readFileSync(new URL('../../src/desktop_bridge.rs', import.meta.url), 'utf8')
    const command = rust.match(/pub\(crate\) const COMMAND: &str = "([a-z-]+)";/)
    expect(command?.[1]).toBe(DEFAULT_BACKEND_ARGS[0])
  })

  test('writes one request line per call and reports closed writes', () => {
    const child = createFakeChild()
    const { channel } = channelFor(child)
    expect(channel.write('{"id":1,"method":"get_status","params":{}}')).toBe(true)
    expect(child.stdin).toBe('{"id":1,"method":"get_status","params":{}}\n')
    channel.close()
    expect(channel.write('{"id":2}')).toBe(false)
  })

  test('delivers stdout lines and forwards stderr diagnostics', async () => {
    const child = createFakeChild()
    const { channel, diagnostics } = channelFor(child)
    const lines: string[] = []
    channel.onMessage(line => lines.push(line))

    child.respond('{"id":1,"protocol":1,"ok":true,"result":{}}')
    child.diagnose('{"bridge":{"kind":"io","message":"stdout write failed"}}')
    await flush()

    expect(lines).toEqual(['{"id":1,"protocol":1,"ok":true,"result":{}}'])
    expect(diagnostics).toHaveLength(1)
    expect(diagnostics[0]).toContain('stdout write failed')
  })

  test('reports an abnormal exit through the close listeners', async () => {
    const child = createFakeChild()
    const { channel } = channelFor(child)
    const reasons: string[] = []
    channel.onClose(reason => reasons.push(reason))
    child.exit(1)
    await flush()
    expect(reasons).toEqual(['desktop bridge exited with code 1'])
  })

  test('reports a spawn failure through the close listeners', async () => {
    const child = createFakeChild()
    const { channel } = channelFor(child)
    const reasons: string[] = []
    channel.onClose(reason => reasons.push(reason))
    child.fail(new Error('ENOENT'))
    await flush()
    expect(reasons).toEqual(['could not start the desktop bridge: ENOENT'])
  })

  test('flushes buffered output on stdout end before reporting the close', async () => {
    const child = createFakeChild()
    const { channel } = channelFor(child)
    const events: string[] = []
    channel.onMessage(line => events.push(`line:${line}`))
    channel.onClose(reason => events.push(`close:${reason}`))
    child.respond('{"id":9,"protocol":1,"ok":true,"result":{}}')
    child.endOutput()
    await flush()
    expect(events).toEqual([
      'line:{"id":9,"protocol":1,"ok":true,"result":{}}',
      'close:desktop bridge closed its output',
    ])
  })

  test('close() ends stdin for a graceful backend shutdown', async () => {
    const child = createFakeChild()
    const { channel } = channelFor(child, { shutdownGraceMs: 5 })
    let ended = false
    child.process.stdin?.once('end', () => {
      ended = true
    })
    channel.close()
    await flush()
    expect(ended).toBe(true)
  })

  test('kills a backend that ignores the graceful shutdown', async () => {
    const child = createFakeChild()
    const { channel, diagnostics } = channelFor(child, { shutdownGraceMs: 5 })
    channel.close()
    await new Promise(resolve => setTimeout(resolve, 30))
    expect(child.killed).toBe(true)
    expect(diagnostics.some(message => message.includes('killing it'))).toBe(true)
  })

  test('does not kill a backend that exits within the grace period', async () => {
    const child = createFakeChild()
    const { channel } = channelFor(child, { shutdownGraceMs: 20 })
    channel.close()
    child.exit(0)
    await new Promise(resolve => setTimeout(resolve, 40))
    expect(child.killed).toBe(false)
  })
})
