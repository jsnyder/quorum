# mini-ts

A tiny TypeScript package used as a fixture for quorum's context feature
tests. It mirrors the surface area of `mini-rust` so cross-language
extractors can be exercised against a similar shape.

## Usage

Import the top-level helper and pass it a token plus verification options:

```typescript
import { verifyToken } from "mini-ts";

const result = verifyToken("abc.def.ghi", { allowExpired: false });
```

## Design

Everything lives under `src/`. The entry point re-exports a single
`verifyToken` function from `./auth`. Keeping the surface area small lets
the fixture remain stable across test runs while still giving extractors
real JSDoc blocks to parse.
