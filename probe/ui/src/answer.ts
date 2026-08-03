import type { Answer } from "./bindings";

// `Answer` and `Verdict` are Rust types. Nothing here restates their shape -
// these are three constructors over the generated one, so a verdict the Rust
// stopped having is a type error rather than a string that renders uncoloured.

export const yes = (detail: string): Answer => ({ verdict: "yes", detail });
export const no = (detail: string): Answer => ({ verdict: "no", detail });
export const info = (detail: string): Answer => ({ verdict: "info", detail });

/** Whatever a thrown value can be persuaded to say about itself. */
export function describe(error: unknown): string {
  if (error instanceof Error) {
    return error.name ? `${error.name}: ${error.message}` : error.message;
  }
  return error === undefined || error === null ? "it failed without saying why" : String(error);
}
