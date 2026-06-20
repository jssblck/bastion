# Article Notes

Source: Alexis King, "Names are not type safety" (2020-11-01), https://lexi-lambda.github.io/blog/2020/11/01/names-are-not-type-safety/

## Core Learning

- A wrapper type is not automatically safer because it has a more specific name.
- Newtypes are useful when they encode an invariant, hide representation, provide a distinct instance/trait behavior, redact secrets, rearrange type parameters, or prevent a concrete misuse across a distance.
- Transparent wrappers that are routinely wrapped and unwrapped are often taxonomy, not safety.
- Encapsulation-based safety requires a small trusted module, clear invariants, and no unsafe/public trapdoors around construction.
- Correct-by-construction datatypes are stronger than wrappers that rely on discipline.
- In application code, wrapper boundaries tend to erode over time, so prefer datatypes whose structure enforces the invariant directly.

## Rust Translation

Prefer a private-field checked wrapper when there is an invariant:

```rust
pub struct CheckedSyncPlan(SyncPlan);

pub fn parse_sync_plan(plan: SyncPlan) -> Result<CheckedSyncPlan> {
    // check every invariant, then return CheckedSyncPlan(plan)
}
```

Avoid a public transparent wrapper that only names a role:

```rust
pub struct SurfaceId(pub String);
```

If there is no invariant, use a field name, module, doc comment, or type alias:

```rust
type SurfaceId = String;
```

## Homeport Examples

- `HomeportProfile` is justified because it proves schema version, unique skill IDs, and MCP transport requirements before adapter translation.
- A future `CheckedSyncPlan` would be justified if it proves no raw auth, cookie, or transcript files are scheduled for sync.
- A hypothetical `SurfaceName(String)` with public inner access would not prove much. If all strings are accepted, prefer a field name or alias; if only known surfaces are accepted, use an enum or hide the field and parse from the supported surface list.
- Avoid `DerefMut` on checked wrappers unless mutation cannot break the proof or the value is reparsed before reuse.
