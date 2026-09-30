// Bounded newline-delimited JSON decoder for the Rust companion's stdout.
//
// Mirrors the framing in `src/desktop_bridge.rs`: one JSON object per line
// terminated by `\n` (an optional `\r` is tolerated), diagnostics never appear
// on stdout, and a line over the limit is dropped instead of buffered. The
// decoder never keeps more than `max` bytes of an unfinished line in memory.

/** Largest accepted line, matching the Rust bridge's `MAX_LINE_BYTES`. */
export const MAX_LINE_BYTES = 64 * 1024

export interface LineDecoder {
  /** Feed a decoded chunk and return every complete line it terminated. */
  push(chunk: string): string[]
  /** Flush the remaining unterminated line, if the stream ended. */
  end(): string[]
  /** Lines dropped for exceeding the limit. */
  readonly oversized: number
}

export function createLineDecoder(max = MAX_LINE_BYTES): LineDecoder {
  let buffered = ''
  let dropping = false
  let oversized = 0

  return {
    push(chunk) {
      const lines: string[] = []
      // A chunk that completes an earlier partial line continues it; while an
      // oversized line is being discarded there is nothing to continue.
      let rest = dropping || buffered.length === 0 ? chunk : buffered + chunk
      buffered = ''
      while (rest.length > 0) {
        const newline = rest.indexOf('\n')
        if (newline < 0) {
          if (!dropping) {
            buffered += rest
            if (buffered.length > max) {
              buffered = ''
              dropping = true
              oversized += 1
            }
          }
          break
        }
        const segment = rest.slice(0, newline)
        rest = rest.slice(newline + 1)
        if (dropping) {
          dropping = false
          continue
        }
        const line = segment.endsWith('\r') ? segment.slice(0, -1) : segment
        if (line.length > max) {
          oversized += 1
          continue
        }
        lines.push(line)
      }
      return lines
    },
    end() {
      const line = buffered
      buffered = ''
      const droppingTail = dropping
      dropping = false
      if (droppingTail || line.length === 0 || line.length > max) return []
      return [line]
    },
    get oversized() {
      return oversized
    },
  }
}
