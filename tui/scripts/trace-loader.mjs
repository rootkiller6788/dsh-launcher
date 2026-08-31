// ESM loader hook that records every resolved file:// module URL to the file
// named by $TRACE_LOG. Used to discover which packages `dsh web` actually loads
// during boot + UI serving, so the deep-prune keeps a safe superset.
//
// Usage:
//   TRACE_LOG=/tmp/dsh-trace.txt \
//   NODE_OPTIONS="--experimental-loader=D:/Opencode/dsh-plugin/dsh-tauri/scripts/trace-loader.mjs" \
//   node host/index.js --port 0 --home <tmp>
//
// NODE_OPTIONS propagates to the dsh child (and worker threads), so the whole
// process tree is traced.

import { appendFileSync } from "node:fs";

const LOG = process.env.TRACE_LOG;

export async function resolve(specifier, context, nextResolve) {
  const result = await nextResolve(specifier, context);
  if (LOG && result?.url?.startsWith("file:")) {
    try {
      appendFileSync(LOG, result.url + "\n");
    } catch {
      /* ignore trace failures */
    }
  }
  return result;
}
