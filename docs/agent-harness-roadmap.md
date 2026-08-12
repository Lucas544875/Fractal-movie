# Agent Harness Roadmap

## Goal

The renderer is being reorganized from a command-oriented application into a
deterministic execution harness for autonomous agents. The first milestone is
complete when an agent can:

1. import the Alchemy scene into a project;
2. create a validated camera-route revision without editing the source scene;
3. render and compare five representative preview frames;
4. promote the selected revision;
5. start, inspect, pause, resume, cancel, and restart a final render at an 80%
   GPU duty-cycle cap; and
6. encode either the complete result or the contiguous frames currently
   available.

The human CLI remains supported. Its preview, static render, animation render,
sequence validation, and FFmpeg execution now delegate to the same workflow
services used by the harness.

## Implementation status

The first harness slice is implemented in `renderer-workflow` and
`renderer-harness`: public `SceneSpec`, immutable candidate revisions,
transactional patch/promotion, idempotent JSON tools, DE target/route
inspection, preview metrics/contact sheets, revision-bound render sequences,
persisted controllable jobs, and independent complete/range/available encode.

The existing `fractal-render` command line remains compatible. Preview profile
application, representative-frame selection, scene sampling, adapter policy,
pipeline reuse, frame progress, and GPU execution now converge on
`FrameRenderSession` in `renderer-workflow`. Sequence frame naming, complete
PNG validation, FFmpeg preflight/arguments, cancellation, temporary output,
and atomic publication converge on the workflow encoder as well. The CLI now
contains transport, compatibility overrides, and human-readable reporting,
not a second renderer or encoder implementation.

## Architectural boundaries

```text
agent
  -> renderer-harness (JSON tool protocol, policy, structured errors)
  -> renderer-workflow (projects, revisions, jobs, artifacts, evaluation)
  -> renderer-core (scene domain, path planning, GPU rendering)
  -> wgpu / PNG / FFmpeg
```

Business logic must not be implemented in protocol handlers. JSON-RPC, MCP,
and the human CLI are replaceable adapters. Scene revisions are immutable and
content-addressed. Every render and encode is bound to one revision hash.

## Priority rules

Priority is not based only on immediate user visibility. A recommended feature
is promoted when postponing it would force a schema, shader interface, artifact
format, or job-model migration.

### P0 — first milestone

1. Public, versioned `SceneSpec` independent from YAML transport.
2. Project store with immutable revisions and optimistic concurrency.
3. Transactional typed patch operations and parameter metadata.
4. Run manifests, artifact descriptors, structured warnings and errors.
5. Long-lived asynchronous jobs with progress, pause, resume, cancellation,
   and bounded resource policy.
6. Scene-hash-aware frame resume to prevent mixed revisions.
7. Preview profiles, representative-frame selection, image metrics, and
   contact sheets exposed through workflow APIs.
8. Independent final-render and encode jobs.
9. Complete and contiguous-partial sequence encoding.
10. Newline-delimited JSON harness with discoverable tool schemas.
11. Preserve the existing CLI contract while the harness uses the workflow
    layer. Preview, render, sequence validation, and FFmpeg execution are
    migrated to shared workflow services.
12. End-to-end Alchemy milestone test that does not require a GPU, plus ignored
    GPU/FFmpeg integration tests.

### P0 hardening — current next work

1. Add protocol-level subprocess tests that exercise the JSONL server over
   stdin/stdout rather than calling its handler directly.
2. Add persisted recovery tests covering process interruption during preview,
   render, and encode publication.
3. Make job manifest publication resilient to a panic in an operation thread,
   so every accepted asynchronous job still reaches a persisted terminal state.
4. Add artifact publication tests for same-filesystem atomic rename and
   cleanup of abandoned temporary files.

### P0 architecture provisions for later work

These interfaces are included in P0 even if their full implementation is not:

- stable IDs for projects, revisions, jobs, artifacts, and future objects;
- artifact `kind` and `media_type` instead of assuming every result is PNG;
- render-pass names in preview requests and metrics manifests;
- parameter `change_cost` (`metadata`, `post-process`, `uniform`, `pipeline`,
  `full-render`, `encode`) so caching can be added without changing the tool
  protocol;
- job resource budgets rather than a GPU-only throttle;
- route planning and resolved-route results as separate concepts;
- a `SceneSpec` top-level schema that can evolve from one fractal to an object
  collection in a new scene version.

### P1 — implement next because deferral is expensive

1. Separate geometry, material, environment, and output schemas.
2. Move material and palette values out of generated shader constants where
   possible, so look development does not rebuild pipelines.
3. Preserve linear HDR output and run tone mapping/post-processing as a
   separately cacheable stage.
4. Add auxiliary hit-mask, depth, normal, object-ID, and march-cost passes.
5. Introduce flat, ID-addressed `objects[]`, transforms, bounds, and bounded
   CSG operations before adding hierarchy.
6. Introduce ID-addressed `materials{}` and `lights[]`; retain the current
   single directional light as a migration shorthand.
7. Represent accepted target-search and route results explicitly rather than
   replanning them whenever a scene is loaded.
8. General camera/light/material keyframes and spline routes with collision,
   visibility, speed, and roll validation.

Items 1–8 are prioritized above automated optimization because each changes
the cache key or scene schema used by every later feature.

### P1 — additive workflow capabilities

1. Multi-revision contact sheets and low-resolution playblasts.
2. Bounded parameter grids with candidate promotion.
3. Temporal flicker, subject coverage, clipping, and motion continuity metrics.
4. Failed-frame retry and explicit frame ranges.
5. Storage quotas, retention policies, and artifact cleanup.
6. MCP adapter returning image content alongside structured JSON.

### P2 — advanced autonomous production

- multi-object material-aware route search;
- Bayesian/evolutionary multi-objective tuning;
- tile/sample checkpoints and distributed rendering;
- OpenEXR and high-precision compositing;
- geometry/material plugin registry;
- learned aesthetic or reference-image similarity metrics.

## Compatibility and migration

- Scene version 1 remains readable and writable.
- The current CLI syntax remains accepted during the P0 migration.
- Source scenes in `scenes/` are never mutated by harness operations.
- A patch creates a candidate revision without moving the project pointer by
  default. A revision is promoted only after full validation and an optional
  optimistic check of the previously current revision.
- Existing PNG sequences without a manifest may be imported, but cannot be
  resumed as a trusted revision until an explicit manifest is created.
- Complete encoding remains the default; partial encoding is always explicit.
- No tool accepts a shell command. FFmpeg settings are passed as typed fields
  and invoked without a shell.

## Completion gates

P0 is complete only when:

- malformed or conflicting patches leave the project unchanged;
- changing a scene revision prevents accidental resume into older frames;
- every asynchronous operation has a persisted terminal status;
- the harness emits one JSON response for every request and never mixes human
  log text into stdout;
- preview and render artifacts can be traced back to an exact revision;
- all unit/workspace tests and Clippy pass; and
- the documented Alchemy tool-call sequence completes on a GPU host.
