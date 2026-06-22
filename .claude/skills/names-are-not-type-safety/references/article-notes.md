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
pub struct CheckedConfig(Config);

pub fn parse_config(config: Config) -> Result<CheckedConfig> {
    // check every invariant, then return CheckedConfig(config)
}
```

Avoid a public transparent wrapper that only names a role:

```rust
pub struct UserId(pub String);
```

If there is no invariant, use a field name, module, doc comment, or type alias:

```rust
type UserId = String;
```

## Examples

- A checked config type is justified when it proves a schema version, unique IDs, and required fields before the rest of the system consumes it.
- A checked plan type is justified when it proves something the raw plan does not, such as that no forbidden resource is scheduled.
- A `Label(String)` wrapper with public inner access does not prove much. If all strings are accepted, prefer a field name or alias; if only known values are accepted, use an enum or hide the field and parse from the allowed set.
- Avoid `DerefMut` on checked wrappers unless mutation cannot break the proof or the value is reparsed before reuse.
