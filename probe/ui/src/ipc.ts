// Runs inside a framed document, twice: once plain and once served with
// `default-src 'self'`.
//
// One question, asked in a pair, because a failure under the policy would prove
// nothing on its own - Tauri might simply not reach a subframe at all. Which
// of the two this is is the last segment of its own path, so the two frames are
// one file.
//
// The answer goes back over the custom protocol rather than to the parent page:
// a policy strict enough to break `invoke` may well break `postMessage` too.

import { commands } from "./bindings";
import { describe, no, yes } from "./answer";

const TOKEN = "a token from a framed document";

async function ask() {
  const invoked = await commands.probeIpc(TOKEN);
  return invoked.echoed === TOKEN
    ? yes("")
    : no(`the command echoed ${JSON.stringify(invoked.echoed)}`);
}

ask()
  .catch((error: unknown) => no(`invoke threw ${describe(error)}`))
  .then((answer) =>
    // Relative, so it lands under whichever probe this document was framed as.
    fetch("result", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(answer),
    }),
  )
  // If the policy stopped the report as well as the invoke, the beacon in
  // ipc.html has already said the document at least loaded.
  .catch(() => {});
