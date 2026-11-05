// Checks that openapi-ts-rs output is type-equal to openapi-typescript 6.7.0's.
//
// usage: node compare.mjs [--keep <dir>] [--list <file>] <openapi-ts-rs binary> [<spec>...]
//
// For every spec, both generators run, and a TypeScript file asserts strict type identity
// and mutual assignability for every schema, response, parameter, request body, header,
// path method and operation (keys come from 6.7.0's own loader). One `tsc` run checks them
// all; each error is mapped back to its spec and key. Exits 1 on any difference.
import { execFile } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { pathToFileURL } from "node:url";
import { promisify } from "node:util";

const run = promisify(execFile);
const here = path.dirname(new URL(import.meta.url).pathname);
const { default: load } = await import(`${here}/node_modules/openapi-typescript/dist/load.js`);
const OT_CLI = `${here}/node_modules/openapi-typescript/bin/cli.js`;
const TSC = `${here}/node_modules/.bin/tsc`;

let args = process.argv.slice(2);
let keep, list;
while (args[0]?.startsWith("--")) {
  if (args[0] === "--keep") [, keep, ...args] = args;
  else if (args[0] === "--list") [, list, ...args] = args;
  else throw new Error(`unknown option ${args[0]}`);
}
const [bin, ...specs] = args;
if (list) specs.push(...fs.readFileSync(list, "utf8").split("\n").filter(Boolean));
const work = keep ?? fs.mkdtempSync(path.join(os.tmpdir(), "openapi-ts-rs-compare-"));
fs.mkdirSync(work, { recursive: true });

const METHODS = ["get", "put", "post", "delete", "options", "head", "patch", "trace"];
const SECTIONS = ["schemas", "responses", "parameters", "requestBodies", "headers", "pathItems"];
const q = (s) => JSON.stringify(s);

async function keysOf(spec) {
  const rootURL = pathToFileURL(path.resolve(spec));
  const schemas = {};
  await load(rootURL, { schemas, rootURL, urlCache: new Set(), parameters: {}, discriminators: {}, fetch: globalThis.fetch });
  const keys = [];
  const ops = new Set();
  for (const [id, sub] of Object.entries(schemas)) {
    if (sub.hint !== "OpenAPI3") {
      keys.push(`external[${q(id)}]`);
      continue;
    }
    const at = (rest) => (id === "." ? rest.replace(/^\[("(?:[^"\\]|\\.)*")\]/, (_, k) => JSON.parse(k)) : `external[${q(id)}]${rest}`);
    const doc = sub.schema ?? {};
    for (const section of SECTIONS) {
      for (const name of Object.keys(doc.components?.[section] ?? {})) keys.push(at(`[${q("components")}][${q(section)}][${q(name)}]`));
    }
    for (const root of ["paths", "webhooks"]) {
      for (const [url, item] of Object.entries(doc[root] ?? {})) {
        for (const m of METHODS) {
          if (!item?.[m]) continue;
          keys.push(at(`[${q(root)}][${q(url)}][${q(m)}]`));
          if (item[m].operationId) ops.add(item[m].operationId);
        }
      }
    }
    for (const name of Object.keys(doc.$defs ?? {})) keys.push(at(`[${q("$defs")}][${q(name)}]`));
  }
  for (const op of ops) keys.push(`operations[${q(op)}]`);
  return keys;
}

async function pool(items, n, fn) {
  const out = new Array(items.length);
  let i = 0;
  await Promise.all(Array.from({ length: n }, async () => {
    while (i < items.length) {
      const j = i++;
      out[j] = await fn(items[j], j);
    }
  }));
  return out;
}

const results = await pool(specs, os.availableParallelism(), async (spec, i) => {
  const dir = path.join(work, String(i));
  fs.mkdirSync(dir, { recursive: true });
  const r = { spec, dir, keys: [], failed: new Set(), status: "ok" };
  try {
    await run("node", [OT_CLI, path.resolve(spec), "-o", `${dir}/old.d.ts`], { maxBuffer: 1 << 28, timeout: 300_000 });
    if (!fs.existsSync(`${dir}/old.d.ts`)) throw new Error("no output");
  } catch (e) {
    r.status = "baseline-failed";
    r.error = String(e.stderr || e.message).trim().split("\n").slice(-3).join(" ");
    return r;
  }
  try {
    await run(bin, [path.resolve(spec), "-o", `${dir}/new.d.ts`], { maxBuffer: 1 << 28, timeout: 300_000 });
  } catch (e) {
    r.status = "new-failed";
    r.error = String(e.stderr || e.message).trim().split("\n").slice(0, 4).join(" ");
    return r;
  }
  try {
    // 6.7.0 omits external entries that render empty; skip keys it never emitted.
    const oldText = fs.readFileSync(`${dir}/old.d.ts`, "utf8");
    r.keys = (await keysOf(spec)).filter((k) => {
      const m = k.match(/^external\[("(?:[^"\\]|\\.)*")\]/);
      return !m || oldText.includes(`\n  ${m[1]}: `);
    });
  } catch (e) {
    r.status = "keys-failed";
    r.error = String(e.message);
    return r;
  }
  const lines = [
    `import type * as O from "./old";`,
    `import type * as N from "./new";`,
    "type Ident<A, B> = (<T>() => T extends A ? 1 : 2) extends (<T>() => T extends B ? 1 : 2) ? true : false;",
    "type Mutual<A, B> = [A] extends [B] ? ([B] extends [A] ? true : false) : false;",
    "type Assert<T extends true> = T;",
  ];
  r.keys.forEach((k, n) => lines.push(`type I${n} = Assert<Ident<N.${k}, O.${k}>>; type M${n} = Assert<Mutual<N.${k}, O.${k}>>;`));
  fs.writeFileSync(`${dir}/check.ts`, lines.join("\n") + "\n");
  return r;
});

// One tsc run per spec: a syntax error anywhere makes tsc skip type checking of the whole program.
const TSCONFIG = JSON.stringify({
  compilerOptions: { strict: true, noEmit: true, module: "esnext", moduleResolution: "bundler", target: "es2022", skipLibCheck: true, types: [] },
  files: ["check.ts"],
});
await pool(results.filter((r) => r.status === "ok"), os.availableParallelism(), async (r) => {
  fs.writeFileSync(`${r.dir}/tsconfig.json`, TSCONFIG);
  let out = "";
  try {
    await run(TSC, ["-p", "tsconfig.json"], { cwd: r.dir, maxBuffer: 1 << 30 });
  } catch (e) {
    out = String(e.stdout) + String(e.stderr);
  }
  const lines = out.split("\n").filter((l) => / error TS/.test(l));
  if (lines.some((l) => l.startsWith("old.d.ts("))) {
    r.status = "baseline-invalid";
    r.error = "6.7.0's output is not valid TypeScript";
    return;
  }
  const check = fs.readFileSync(`${r.dir}/check.ts`, "utf8").split("\n");
  for (const l of lines) {
    const m = l.match(/^check\.ts\((\d+),/);
    if (m) r.failed.add(r.keys[Number(check[Number(m[1]) - 1].match(/^type I(\d+)/)[1])]);
    else r.failed.add(`<tsc: ${l.slice(0, 160)}>`);
  }
});

// Specs reviewed to differ only because of a 6.7.0 bug (known-differences.tsv) are reported, not failed.
const known = new Map(
  fs.readFileSync(`${here}/known-differences.tsv`, "utf8").split("\n").filter((l) => l && !l.startsWith("#")).map((l) => l.split("\t")),
);
for (const r of results) {
  const why = known.get(path.basename(r.spec));
  if (why && r.status === "ok" && r.failed.size) {
    r.status = "known";
    r.error = `differs on ${r.failed.size}/${r.keys.length} keys because of 6.7.0 bug ${why}`;
  }
}

let total = 0, matched = 0, bad = 0;
for (const r of results) {
  const n = r.keys.length;
  total += n;
  matched += n - r.failed.size;
  const ok = r.status === "ok" && r.failed.size === 0;
  const skipped = ["baseline-failed", "baseline-invalid", "known"].includes(r.status);
  if (!ok) bad++;
  const label = r.status === "ok" ? `${n - r.failed.size}/${n} keys equal` : `${r.status}: ${r.error}`;
  console.log(`${ok ? "ok  " : skipped ? "skip" : "FAIL"} ${r.spec}: ${label}`);
  if (skipped) continue;
  for (const k of [...r.failed].slice(0, 20)) console.log(`       ${k}`);
  if (r.failed.size > 20) console.log(`       ... ${r.failed.size - 20} more`);
}
const count = (st) => results.filter((r) => r.status === st).length;
console.log(`\n${specs.length - bad}/${specs.length} specs equal, ${matched}/${total} keys equal; ${count("known")} known 6.7.0 bugs, ${count("baseline-failed") + count("baseline-invalid")} specs 6.7.0 cannot handle. Work dir: ${work}`);
process.exitCode = results.some((r) => !["baseline-failed", "baseline-invalid", "known"].includes(r.status) && (r.status !== "ok" || r.failed.size)) ? 1 : 0;
