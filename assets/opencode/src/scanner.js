// Quote-aware shell scanning (issue #541 P1; heredoc-opaque issue #859 P2).
//
// Inside `'…'` or `"…"` a separator is data, not syntax, and outside single
// quotes a backslash escapes the next character. An unterminated quote owns
// the rest of the text.
//
// Heredoc bodies (`<<` / `<<-` with quoted, unquoted, or quote-removed
// delimiters, multiple queued bodies in order) are opaque data: body bytes
// (apostrophes, quotes, semicolons, `phasegent` look-alikes, role claims)
// never pollute quote state, never split segments, and never rewrite.
// Here-strings (`<<<`) are not heredocs. Operators inside quoted strings or
// `#` comments are not declarations. Unterminated heredocs and ambiguous
// quote shapes conservatively preserve the affected text and rely on the
// execution-layer role/marker instead of inventing shell semantics.

export function skipQuoted(text, start) {
  const quote = text[start];
  let index = start + 1;
  while (index < text.length) {
    const char = text[index];
    if (quote === '"' && char === "\\") {
      index += 2;
      continue;
    }
    if (char === quote) return index + 1;
    index += 1;
  }
  return -1;
}

function isCommentStart(text, index) {
  if (text[index] !== "#") return false;
  if (index === 0) return true;
  const prev = text[index - 1];
  return (
    prev === " " ||
    prev === "\t" ||
    prev === "\n" ||
    prev === ";" ||
    prev === "&" ||
    prev === "|" ||
    prev === "("
  );
}

function isHeredocWordTerminator(char) {
  return (
    char === undefined ||
    char === " " ||
    char === "\t" ||
    char === "\n" ||
    char === ";" ||
    char === "&" ||
    char === "|" ||
    char === "(" ||
    char === ")" ||
    char === "<" ||
    char === ">" ||
    char === "#"
  );
}

// Scan one delimiter word starting at `pos` (after `<<[-]` plus blanks).
// Returns `{ value, end }` where `value` is the quote-removed delimiter for
// terminator comparison and `end` is the index after the word in original
// bytes, or null when there is no usable delimiter (unsupported shape).
function parseHeredocDelimiter(text, pos) {
  const n = text.length;
  if (pos >= n || isHeredocWordTerminator(text[pos])) return null;
  let value = "";
  let index = pos;
  while (index < n && !isHeredocWordTerminator(text[index])) {
    const char = text[index];
    if (char === "'") {
      const close = text.indexOf("'", index + 1);
      // Unterminated quote inside the delimiter: ambiguous, not a heredoc.
      // Also reject a closing quote that runs past the word (newline).
      if (close < 0) return null;
      const newlineBeforeClose = text.indexOf("\n", index + 1);
      if (newlineBeforeClose >= 0 && newlineBeforeClose < close) return null;
      value += text.slice(index + 1, close);
      index = close + 1;
      continue;
    }
    if (char === '"') {
      let cursor = index + 1;
      let inner = "";
      let closed = false;
      while (cursor < n) {
        const innerChar = text[cursor];
        if (innerChar === "\\" && cursor + 1 < n && text[cursor + 1] !== "\n") {
          inner += text[cursor + 1];
          cursor += 2;
          continue;
        }
        if (innerChar === '"') {
          closed = true;
          cursor += 1;
          break;
        }
        if (innerChar === "\n") break;
        inner += innerChar;
        cursor += 1;
      }
      if (!closed) return null;
      value += inner;
      index = cursor;
      continue;
    }
    if (char === "\\") {
      if (index + 1 >= n || text[index + 1] === "\n") return null;
      value += text[index + 1];
      index += 2;
      continue;
    }
    value += char;
    index += 1;
  }
  if (value.length === 0) return null;
  return { value, end: index };
}

function terminatorLineMatches(text, lineStart, lineEnd, delimiter, stripTabs) {
  // Strip a trailing `\r` for `CRLF` scripts without changing byte offsets:
  // matching uses content without `\r`, ranges still cover it.
  let contentEnd = lineEnd;
  if (contentEnd > lineStart && text[contentEnd - 1] === "\r") contentEnd -= 1;
  let pos = lineStart;
  if (stripTabs) {
    while (pos < contentEnd && text[pos] === "\t") pos += 1;
  } else if (pos < contentEnd && (text[pos] === " " || text[pos] === "\t")) {
    return false;
  }
  if (pos + delimiter.length !== contentEnd) return false;
  return text.slice(pos, pos + delimiter.length) === delimiter;
}

// Shared lexical scan: opaque heredoc ranges (delimiter words plus bodies
// with terminator lines, excluding the final newline so the terminator
// newline still separates commands). Sorted, non-overlapping. Empty when
// there is no `<<` heredoc declaration in code.
export function scanHeredocRanges(text) {
  const ranges = [];
  const n = text.length;
  if (!text.includes("<<")) return ranges;
  let index = 0;
  let inSingle = false;
  let inDouble = false;
  let inComment = false;
  // Declarations collected on the current logical line (before its newline).
  let pending = [];
  // Delimiter word ranges for the current line (pushed immediately so quote
  // removal literals in data stay untouched by later transforms).
  const pushRange = (start, end) => {
    if (end > start) ranges.push({ start, end });
  };

  const consumePendingBodies = (newlineIndex) => {
    // Called on an unescaped, unquoted newline with pending declarations.
    // The declaration newline itself is heredoc syntax (not a separator), so
    // the first body range includes it; intermediate terminator newlines are
    // included to keep queued bodies contiguous, while the final terminator
    // newline stays outside so it still separates the following command.
    let bodyPos = newlineIndex;
    for (let declIndex = 0; declIndex < pending.length; declIndex += 1) {
      const decl = pending[declIndex];
      const isLast = declIndex === pending.length - 1;
      const bodyStart = bodyPos;
      const searchFrom = declIndex === 0 ? newlineIndex + 1 : bodyPos;
      let lineStart = searchFrom;
      let found = false;
      while (lineStart <= n) {
        if (lineStart >= n) {
          // Unterminated: rest is opaque body, no terminator.
          pushRange(bodyStart, n);
          bodyPos = n;
          found = false;
          break;
        }
        const newlineAt = text.indexOf("\n", lineStart);
        const lineEnd = newlineAt < 0 ? n : newlineAt;
        if (terminatorLineMatches(text, lineStart, lineEnd, decl.delimiter, decl.stripTabs)) {
          if (isLast) {
            pushRange(bodyStart, lineEnd);
            bodyPos = newlineAt < 0 ? n : newlineAt + 1;
          } else {
            pushRange(bodyStart, newlineAt < 0 ? n : newlineAt + 1);
            bodyPos = newlineAt < 0 ? n : newlineAt + 1;
          }
          found = true;
          break;
        }
        lineStart = newlineAt < 0 ? n : newlineAt + 1;
      }
      if (!found && bodyPos >= n) {
        // Unterminated rest already pushed; remaining queued bodies share it.
        break;
      }
    }
    pending = [];
    return bodyPos;
  };

  while (index < n) {
    const char = text[index];
    if (inComment) {
      if (char === "\n") {
        inComment = false;
        // A comment newline ends the line: bodies follow if declarations
        // preceded the comment on the same line (e.g. `cat <<EOF # comment`).
        if (pending.length > 0 && !inSingle && !inDouble) {
          const next = consumePendingBodies(index);
          // Skip the opaque bodies (and terminators) without re-scanning
          // their quotes, separators, or fake declarations.
          index = next;
          continue;
        }
      }
      index += 1;
      continue;
    }
    if (inSingle) {
      if (char === "'") inSingle = false;
      index += 1;
      continue;
    }
    if (inDouble) {
      if (char === "\\" && index + 1 < n) {
        index += 2;
        continue;
      }
      if (char === '"') inDouble = false;
      index += 1;
      continue;
    }
    // In code.
    if (char === "\\" && index + 1 < n) {
      // Escaped char (including an escaped newline line-continuation):
      // never an operator, quote, or line end.
      index += 2;
      continue;
    }
    if (char === "'") {
      inSingle = true;
      index += 1;
      continue;
    }
    if (char === '"') {
      inDouble = true;
      index += 1;
      continue;
    }
    if (char === "#" && isCommentStart(text, index)) {
      inComment = true;
      index += 1;
      continue;
    }
    if (char === "<" && text[index + 1] === "<") {
      if (text[index + 2] === "<") {
        // Here-string `<<<`: not a heredoc, skip the operator.
        index += 3;
        continue;
      }
      let cursor = index + 2;
      let stripTabs = false;
      if (text[cursor] === "-") {
        stripTabs = true;
        cursor += 1;
      }
      let blank = cursor;
      while (blank < n && (text[blank] === " " || text[blank] === "\t")) blank += 1;
      const parsed = parseHeredocDelimiter(text, blank);
      if (!parsed) {
        // Unsupported shape (`<<` with no word, empty removal, or
        // unterminated quote in the word): not a heredoc, keep bytes.
        index += 2 + (stripTabs ? 1 : 0);
        continue;
      }
      // Valid declaration on this line: queue its body, mark its word opaque.
      pending.push({ delimiter: parsed.value, stripTabs });
      pushRange(blank, parsed.end);
      index = parsed.end;
      continue;
    }
    if (char === "\n") {
      if (pending.length > 0) {
        const next = consumePendingBodies(index);
        index = next;
        continue;
      }
      index += 1;
      continue;
    }
    index += 1;
  }
  ranges.sort((a, b) => a.start - b.start);
  return ranges;
}

// Replace every quoted run (and escaped character) with spaces of the same
// length: offsets still line up with the original command, and a quoted value
// can never be read as a flag. `balanced` is false when a quote is never closed.
// Heredoc bodies and delimiter words are opaque first, so an apostrophe in
// data never starts a quote that swallows a real invocation.
export function maskQuoted(text) {
  const heredoc = scanHeredocRanges(text);
  let masked = "";
  let index = 0;
  let balanced = true;
  let heredocPos = 0;
  const skipHeredoc = () => {
    while (heredocPos < heredoc.length && index >= heredoc[heredocPos].end) heredocPos += 1;
    if (heredocPos < heredoc.length) {
      const range = heredoc[heredocPos];
      if (index >= range.start && index < range.end) return range;
    }
    return null;
  };
  while (index < text.length) {
    const opaque = skipHeredoc();
    if (opaque) {
      masked += " ".repeat(opaque.end - index);
      index = opaque.end;
      continue;
    }
    const char = text[index];
    if (char === "'" || char === '"') {
      const end = skipQuoted(text, index);
      if (end < 0) {
        masked += " ".repeat(text.length - index);
        balanced = false;
        break;
      }
      masked += " ".repeat(end - index);
      index = end;
      continue;
    }
    if (char === "\\") {
      const width = index + 1 < text.length ? 2 : 1;
      masked += " ".repeat(width);
      index += width;
      continue;
    }
    masked += char;
    index += 1;
  }
  return { masked, balanced };
}

// Apply `transform` to the code spans only; quoted runs, escaped characters,
// and heredoc bodies/delimiters stay byte-for-byte, so a rewrite never lands
// inside a value or data.
export function transformCodeOnly(text, transform) {
  const heredoc = scanHeredocRanges(text);
  let result = "";
  let cursor = 0;
  let index = 0;
  let heredocPos = 0;
  const opaqueAt = () => {
    while (heredocPos < heredoc.length && index >= heredoc[heredocPos].end) heredocPos += 1;
    if (heredocPos < heredoc.length) {
      const range = heredoc[heredocPos];
      if (index >= range.start && index < range.end) return range;
    }
    return null;
  };
  while (index < text.length) {
    const opaque = opaqueAt();
    if (opaque) {
      result += transform(text.slice(cursor, index)) + text.slice(index, opaque.end);
      cursor = opaque.end;
      index = opaque.end;
      continue;
    }
    const char = text[index];
    if (char === "'" || char === '"') {
      const end = skipQuoted(text, index);
      if (end < 0) {
        result += transform(text.slice(cursor, index)) + text.slice(index);
        cursor = text.length;
        break;
      }
      result += transform(text.slice(cursor, index)) + text.slice(index, end);
      cursor = end;
      index = end;
      continue;
    }
    if (char === "\\") {
      index += 2;
      continue;
    }
    index += 1;
  }
  return result + transform(text.slice(cursor));
}

export function separatorAt(command, index) {
  const pair = command.slice(index, index + 2);
  if (pair === "&&" || pair === "||") return pair;
  const char = command[index];
  if (char === "&") {
    // Redirection, not a segment boundary: `&>`/`&>>` (the `&` sits before
    // `>`) and `N>&M`/`>&N`/`<&N` (the `&` sits after `>`/`<`). Only a
    // standalone `&` is the background separator (issue #544 P1-b).
    if (command[index + 1] === ">" || command[index - 1] === ">" || command[index - 1] === "<") {
      return null;
    }
    return "&";
  }
  if (char === ";" || char === "|" || char === "(" || char === ")" || char === "\n") {
    return char;
  }
  return null;
}

export function shellSegments(command) {
  const heredoc = scanHeredocRanges(command);
  let heredocPos = 0;
  const opaqueAt = (pos) => {
    while (heredocPos < heredoc.length && pos >= heredoc[heredocPos].end) heredocPos += 1;
    // Note: `heredocPos` advances monotonically with `pos` because callers
    // only move forward; ranges are sorted so this stays consistent.
    for (const range of heredoc) {
      if (pos >= range.start && pos < range.end) return range;
    }
    return null;
  };
  const segments = [];
  let cursor = 0;
  let index = 0;
  while (index < command.length) {
    const opaque = opaqueAt(index);
    if (opaque) {
      index = opaque.end;
      continue;
    }
    const char = command[index];
    if (char === "'" || char === '"') {
      const end = skipQuoted(command, index);
      if (end < 0) break; // unterminated quote: the rest belongs to this segment
      index = end;
      continue;
    }
    if (char === "\\") {
      index += 2;
      continue;
    }
    const separator = separatorAt(command, index);
    if (separator === null) {
      index += 1;
      continue;
    }
    if (index > cursor) {
      segments.push({ start: cursor, end: index, text: command.slice(cursor, index) });
    }
    cursor = index + separator.length;
    index = cursor;
    // A separator may land inside a heredoc terminator gap? No: heredoc
    // ranges exclude the terminator newline, so the newline after the
    // terminator is a real separator and correctly splits here.
    heredocPos = 0;
  }
  if (cursor < command.length) {
    segments.push({ start: cursor, end: command.length, text: command.slice(cursor) });
  }
  return segments;
}

// One whitespace-delimited word of the masked segment starting at `pos`.
// Quoted runs are already blanks in `masked`, so a value can never be read as
// a flag; offsets stay aligned with the original segment.
function nextShellToken(masked, pos) {
  const n = masked.length;
  let index = pos;
  while (index < n && (masked[index] === " " || masked[index] === "\t")) index += 1;
  if (index >= n) return null;
  let end = index;
  while (end < n && masked[end] !== " " && masked[end] !== "\t") end += 1;
  return { word: masked.slice(index, end), start: index, end };
}

const ASSIGNMENT_WORD = /^[A-Za-z_][A-Za-z0-9_]*=/;

// `timeout` option tail: bounded arity before its command word. `null` means
// an unsupported/ambiguous shape, so the caller leaves the bytes untouched.
function parseTimeoutWrapper(masked, pos) {
  let index = pos;
  let durationSeen = false;
  for (let guard = 0; guard < 32; guard += 1) {
    const token = nextShellToken(masked, index);
    if (!token) return null;
    const word = token.word;
    if (word === "--") {
      return durationSeen ? token.end : null;
    }
    if (word === "-s" || word === "--signal" || word === "-k" || word === "--kill-after") {
      const value = nextShellToken(masked, token.end);
      if (!value) return null;
      index = value.end;
      continue;
    }
    if (word.startsWith("--signal=") || word.startsWith("--kill-after=")) {
      index = token.end;
      continue;
    }
    if (
      word === "--preserve-status" ||
      word === "--foreground" ||
      word === "-v" ||
      word === "--verbose"
    ) {
      index = token.end;
      continue;
    }
    if (word.startsWith("-")) return null;
    if (!durationSeen) {
      durationSeen = true;
      index = token.end;
      continue;
    }
    return token.start;
  }
  return null;
}

// `env` option/assignment tail up to its command word (or null when
// unsupported).
function parseEnvWrapper(masked, pos) {
  let index = pos;
  for (let guard = 0; guard < 64; guard += 1) {
    const token = nextShellToken(masked, index);
    if (!token) return null;
    const word = token.word;
    if (word === "-i" || word === "--ignore-environment" || word === "--null") {
      index = token.end;
      continue;
    }
    if (
      word === "-u" ||
      word === "--unset" ||
      word === "-C" ||
      word === "--chdir" ||
      word === "-S" ||
      word === "--split-string"
    ) {
      const value = nextShellToken(masked, token.end);
      if (!value) return null;
      index = value.end;
      continue;
    }
    if (
      word.startsWith("--unset=") ||
      word.startsWith("--chdir=") ||
      word.startsWith("--split-string=")
    ) {
      index = token.end;
      continue;
    }
    if (ASSIGNMENT_WORD.test(word)) {
      index = token.end;
      continue;
    }
    if (word.startsWith("-")) return null;
    return token.start;
  }
  return null;
}

// Bounded POSIX transparent wrappers (`timeout`, `env`, `command`) plus
// assignment words before the actual command name (issue #917 P1). Returns the
// byte offset where the command word starts and whether any wrapper was
// crossed; `null` means an unsupported/ambiguous shape, so the invocation is
// left byte-for-byte instead of guessing shell semantics.
function parseWrappers(masked, pos) {
  let index = pos;
  let wrapped = false;
  for (let guard = 0; guard < 16; guard += 1) {
    const token = nextShellToken(masked, index);
    if (!token) return null;
    if (ASSIGNMENT_WORD.test(token.word)) {
      index = token.end;
      continue;
    }
    if (token.word === "timeout") {
      const after = parseTimeoutWrapper(masked, token.end);
      if (after === null) return null;
      index = after;
      wrapped = true;
      continue;
    }
    if (token.word === "env") {
      const after = parseEnvWrapper(masked, token.end);
      if (after === null) return null;
      index = after;
      wrapped = true;
      continue;
    }
    if (token.word === "command") {
      // `command -v`/`-V` are lookups, not executions: any option makes the
      // shape ambiguous, so it stays untouched.
      const next = nextShellToken(masked, token.end);
      if (!next || next.word.startsWith("-")) return null;
      index = token.end;
      wrapped = true;
      continue;
    }
    return { pos: token.start, wrapped };
  }
  return null;
}

export function phasegentInvocation(segment) {
  const { masked, balanced } = maskQuoted(segment.text);
  let text = segment.text;
  let offset = segment.start;
  const leading = text.match(/^\s+/);
  if (leading) {
    offset += leading[0].length;
    text = text.slice(leading[0].length);
  }
  const env = text.match(/^(?:[A-Za-z_][A-Za-z0-9_]*=(?:"[^"]*"|'[^']*'|\S+)\s+)+/);
  if (env) {
    offset += env[0].length;
    text = text.slice(env[0].length);
  }
  // Transparent wrappers (`timeout 40s env FOO=1 phasegent …`) sit between the
  // leading assignments and the command name. The wrapper context is recorded
  // so the rewrite can inject a valid invocation-local env (`timeout … env
  // PHASEGENT_ROLE=… phasegent`), which an assignment inserted as an
  // executable would not be.
  const wrappers = parseWrappers(masked, offset - segment.start);
  if (wrappers === null) return null;
  const wrapped = wrappers.wrapped;
  offset = segment.start + wrappers.pos;
  text = segment.text.slice(wrappers.pos);
  // The command name must be a whole word followed by whitespace or the
  // segment end, and the prefix slot excludes quotes and backticks: `\b` also
  // matched before `-`/`.`/`:`/`/`, so a quoted path such as
  // `assets/opencode/phasegent-worktree.js` was rewritten as a call
  // (issue #544 P1-a).
  const token = text.match(/^(?:[^\s;&|()'"`]*\/)?phasegent(?=\s|$)/);
  if (!token) return null;
  const trailing = text.length - text.trimEnd().length;
  return {
    tokenEnd: offset + token[0].length,
    segmentEnd: segment.end - trailing,
    tail: masked.slice(offset - segment.start + token[0].length),
    balanced,
    wrapped,
  };
}
