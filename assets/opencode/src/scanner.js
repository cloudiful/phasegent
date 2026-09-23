// Quote-aware shell scanning (issue #541 P1).
//
// Inside `'…'` or `"…"` a separator is data, not syntax, and outside single
// quotes a backslash escapes the next character. An unterminated quote owns
// the rest of the text.

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

// Replace every quoted run (and escaped character) with spaces of the same
// length: offsets still line up with the original command, and a quoted value
// can never be read as a flag. `balanced` is false when a quote is never closed.
export function maskQuoted(text) {
  let masked = "";
  let index = 0;
  let balanced = true;
  while (index < text.length) {
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

// Apply `transform` to the code spans only; quoted runs and escaped characters
// stay byte-for-byte, so a rewrite never lands inside a value.
export function transformCodeOnly(text, transform) {
  let result = "";
  let cursor = 0;
  let index = 0;
  while (index < text.length) {
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
  const segments = [];
  let cursor = 0;
  let index = 0;
  while (index < command.length) {
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
  }
  if (cursor < command.length) {
    segments.push({ start: cursor, end: command.length, text: command.slice(cursor) });
  }
  return segments;
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
  };
}
