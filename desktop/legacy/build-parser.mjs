import { build } from "esbuild"
import { fileURLToPath } from "node:url"
import path from "node:path"

const root = fileURLToPath(new URL("../../", import.meta.url))
const desktop = fileURLToPath(new URL("../", import.meta.url))

// Bundle the original PEV2 service and its dependencies for the embedded JS
// engine in Tauri. The frontend never imports or executes the parser.
await build({
  entryPoints: [path.join(desktop, "parser-entry.ts")],
  outfile: path.join(desktop, "src-tauri/src/plan_parser.js"),
  bundle: true,
  minify: true,
  format: "iife",
  platform: "neutral",
  target: "es2020",
  plugins: [{
    name: "pev2-source-alias",
    setup(build) {
      build.onResolve({ filter: /^@\// }, args => ({ path: path.join(root, "src", `${args.path.slice(2)}.ts`) }))
    },
  }],
})
