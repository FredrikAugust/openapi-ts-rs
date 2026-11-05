# openapi-ts-rs

Generates TypeScript types from OpenAPI 3 specs in the shape
[openapi-typescript](https://github.com/openapi-ts/openapi-typescript) 6.7.0 emits (`paths`,
`components`, `operations`, `external`), so code written against 6.7.0's output keeps compiling.
It is a native binary with no Node or TypeScript dependency, so it is unaffected by TypeScript 7.

```sh
openapi-ts-rs spec.yml -o spec.gen.d.ts   # warns about $refs that do not resolve
openapi-ts-rs spec.yml --strict           # ...or fails on them
```

## Install with mise

```toml
# mise.toml
[tools]
"github:FredrikAugust/openapi-ts-rs" = "latest"
```

Every push to `main` publishes a release (`v0.1.<run number>`) with binaries for Linux and macOS on
x86_64 and arm64. While the repository is private, mise needs a `GITHUB_TOKEN` that can read it.

## Compatibility with 6.7.0

Output is checked by type equality, not text: `compat/compare.mjs` runs both generators and asks
TypeScript whether every schema, response, parameter, request body, header, path method and
operation is identical (and mutually assignable) between the two outputs.

```sh
cargo build --release
cd compat && npm ci && ./fetch-corpus.sh corpus
node compare.mjs --list corpus/specs.txt ../target/release/openapi-ts-rs
```

The corpus is openapi-typescript 6.7.0's examples and fixtures plus the 1,970 OpenAPI 3 specs in
APIs-guru/openapi-directory at a pinned commit (CI uses every 8th). Output is type-equal to 6.7.0
except where 6.7.0 is wrong; those specs are listed with the bug in `compat/known-differences.tsv`:

- percent-encoded `$ref` pointers are left encoded, so the lookup names a key that does not exist;
- unquoted YAML dates become `Date`s, so `enum: [2011-01-01]` becomes `"2011-01-01T00:00:00.000Z"`;
- string enum values under a non-string `type` are emitted unquoted;
- parameters referenced by array index (`…/parameters/2`) are dropped;
- backslashes in string literals are not escaped;
- non-pointer fragments (`file.yaml#Name`) are ignored.

Other 6.7.0 behavior, odd or not, is kept on purpose, for example duplicate discriminator
properties and `WithRequired` for `allOf` with `required`. Only local files are supported; remote
(`http`) refs and openapi-typescript's CLI flags are not.

## Layout

- `src/yaml.rs`: YAML to `serde_json::Value` from granit-parser events, with js-yaml 4's scalar rules.
- `src/load.rs`: reads the root spec and every file it references (in parallel) and decides how each
  external file is rendered, like 6.7.0's loader.
- `src/emit.rs`, `src/ts.rs`: the TypeScript output, one method per openapi-typescript transform.
