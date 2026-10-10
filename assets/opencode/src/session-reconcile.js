// Reconciling the worktree registry against the host (issue 747 P2).
//
// A lease records where a session *was*, and this registry recorded where the
// plugin put it. Neither survives an authoritative move: `issue close` returns
// the closing session to the repository's main checkout through the OpenCode
// API before it removes the worktree, and the directory a cached entry names can
// be gone by the time the next prompt arrives. So the host is read fresh and
// three rules follow from its answer:
//
//   * a placement this adapter already *settled* is never reversed — the host
//     reporting somewhere else is evidence that something else moved it;
//   * an unreadable host is not evidence that the placement still holds. The
//     directory may already be gone, so the cached target is dropped rather
//     than re-entered on an unproven assumption;
//   * a session that is not settled is left alone, so a session still sitting
//     in the main checkout before its first placement keeps that placement.

import { forgetSessionWorktree, readAuthoritativeDirectory, sessionPlaced, sessionWorktrees } from "./session.js";

// True when the cached target for `sessionId` may still be used.
//
// `readDirectory` is the injection seam; it defaults to the uncached host read
// and a caller must not substitute a cached reader, because answering this
// question from a cache is the failure this module exists to prevent.
export async function reconcileSessionLocation(context, sessionId, readDirectory) {
  if (!sessionPlaced(sessionId)) return true;
  const registered = sessionWorktrees.get(String(sessionId));
  const read = readDirectory || readAuthoritativeDirectory;
  const directory = await read(context, sessionId);
  // A known-closed worktree is never revived on an unproven assumption: the
  // host would not say where the session is, and the directory the registry
  // still names may already have been removed.
  if (!directory || (registered && directory !== registered)) {
    forgetSessionWorktree(sessionId, registered);
    return false;
  }
  return true;
}
