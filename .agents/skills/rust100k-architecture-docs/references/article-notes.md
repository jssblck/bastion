# Article Notes

Source: https://matklad.github.io/2021/02/06/ARCHITECTURE.md.html

Matklad's durable points:

- An architecture document exists to transfer the maintainer's mental map.
- Keep it short and stable; revisit periodically rather than syncing every detail with code.
- Start with a bird's-eye problem overview, then provide a codemap that answers "where is X?" and "what does this thing do?"
- Name important files, modules, and types without creating links that go stale.
- Call out invariants, especially absences that cannot be inferred locally.
- Describe boundaries and cross-cutting concerns.
- Use the doc as a chance to notice when source layout and conceptual layout have drifted.

Homeport adaptation:

- `docs/architecture.md` is already the right home.
- The doc should stay focused on runtime boundaries, module map, adapter contract, skill bundles, local artifact generation, and secret-handling exclusions.
- Detailed product research belongs in `docs/research.md`; implementation-level churn belongs in code comments or narrower docs.
