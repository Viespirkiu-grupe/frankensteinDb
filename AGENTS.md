# Engineering Guidelines

These instructions apply to the entire repository.

## File size and organization

- Source files must not exceed 500 lines.
- Split a file before it reaches the limit. Organize modules by a clear responsibility rather than
  creating arbitrary numbered or generic helper files.
- Keep public APIs and orchestration near the module root; move parsing, storage, query execution,
  aggregation, and other independent concerns into focused submodules.
- Tests should follow the same 500-line limit and should be grouped by the behavior they verify.

## Functions

- Functions must have one clear responsibility and names that describe their purpose.
- Prefer short, linear control flow. Extract complex validation, conversion, branching, or repeated
  logic into well-named helpers.
- Avoid hidden side effects and surprising fallback behavior. Make storage, commit, recovery, and
  query-engine boundaries explicit.
- Keep parameters and return values typed and meaningful. Do not use loosely structured values when
  a small dedicated type would make the contract clearer.

## Documentation

- Document public types, functions, configuration fields, typed API extensions, and non-obvious
  invariants.
- Explain why a complex implementation or constraint exists, not what an obvious line of code does.
- Keep README examples and architecture notes synchronized with behavior and public APIs.
- Document performance-sensitive tradeoffs, durability guarantees, and unsupported behavior.

## Reuse

- Make functions and components reusable whenever practical, without introducing unnecessary
  abstraction.
- Keep canonical data conversion, typed request parsing, Tantivy encoding, mutation handling, and query
  execution as independent components with explicit interfaces.
- Reuse shared validation and conversion functions instead of duplicating schema or type rules.
- Prefer composable helpers over special cases embedded in binaries or benchmarks.

## Verification

- Preserve the invariant that supported reads execute exclusively through Tantivy.
- Add or update focused tests whenever behavior changes.
- Before completing a change, run formatting, the relevant tests, and Clippy with warnings denied.
- Do not run the large dataset benchmark unless the user explicitly asks for it.
