/**
 * Extract the human-readable message from a CommandError.
 *
 * CommandError is `{ kind: ErrorKind, message: string }` coming from the
 * Rust backend. This helper provides a single place to pull the message
 * string for display or logging.
 */
export function errorMessage(error: { kind: string; message: string }): string {
  return error.message;
}

/**
 * Check whether a CommandError represents a cancellation.
 */
export function isCancelledError(error: { kind: string; message: string }): boolean {
  return error.kind === "Cancelled";
}
