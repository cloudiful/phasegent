// Reconciling the worktree registry against the host.
//
// A lease records where a session *was*, and this registry recorded where the
// plugin put it. Neither survives an authoritative move: `issue close` returns
// the closing session to the repository's main checkout through the OpenCode
// API before it removes the worktree, and the directory a cached entry names can
// be gone by the time the next prompt arrives. A move made out of band — the
// OpenCode API, or another operator — is invisible to the registry in exactly
// the same way. So the host is read fresh and the answer decides:
//
//   * the host reports the registered target: the placement is settled and the
//     host confirmed it;
//   * the host reports the recorded origin while the target was never
//     confirmed: the accepted move has not landed yet, so the target stays
//     pending. Only a confirmation ends that tolerance, never the acceptance
//     of the move itself;
//   * the host reports any other directory — a third one, or the origin of an
//     already confirmed target: the session was relocated, so the stale target
//     is dropped, remembered as abandoned, and the host wins;
//   * the host cannot be read: a settled session drops its target, because the
//     directory may already be gone, and an unsettled one keeps it so the move
//     itself fails closed instead of the turn running unplaced.
//
// A registered-but-unsettled session is reconciled too: a target whose settled
// markers are missing used to win unconditionally, so the next prompt moved a
// session the host had already relocated back into the stale directory.

import {
  forgetSessionWorktree,
  hostConfirmed,
  markConfirmed,
  markTargetAbandoned,
  placementOrigin,
  readAuthoritativeDirectory,
  sessionPlaced,
  sessionWorktrees,
} from "./session.js";
import { placementDiagnostic } from "./runtime.js";

// True when the registered target for `sessionId` may still be used.
//
// `readDirectory` is the injection seam; it defaults to the uncached host read
// and a caller must not substitute a cached reader, because answering this
// question from a cache is the failure this module exists to prevent.
export async function reconcileSessionLocation(context, sessionId, readDirectory) {
  const key = sessionId === undefined || sessionId === null ? null : String(sessionId);
  const registered = key === null ? undefined : sessionWorktrees.get(key);
  // A session without a registered target has nothing to reconcile.
  if (!registered) return true;
  const read = readDirectory || readAuthoritativeDirectory;
  const directory = await read(context, sessionId);
  if (!directory) {
    if (sessionPlaced(sessionId)) {
      forgetSessionWorktree(sessionId, registered);
      placementDiagnostic("skip", { session: key, reason: "reconciled-elsewhere" });
      return false;
    }
    // An unsettled placement keeps its target, so the move itself fails closed
    // on the unreadable host instead of the turn running unplaced.
    return true;
  }
  if (directory === registered) {
    // The session sits in its target: the placement is settled and the host
    // confirmed it.
    markConfirmed(sessionId);
    return true;
  }
  const origin = placementOrigin(sessionId);
  if (origin && directory === origin && !hostConfirmed(sessionId)) {
    // The accepted move has not landed yet: the target stays pending.
    return true;
  }
  // The session was relocated. Its target is dropped and remembered as
  // abandoned, so a later lease rediscovery cannot hand it back.
  markTargetAbandoned(sessionId, registered);
  forgetSessionWorktree(sessionId, registered);
  placementDiagnostic("skip", { session: key, reason: "reconciled-elsewhere" });
  return false;
}
