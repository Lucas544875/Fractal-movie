# Agent Harness

`fractal-harness` is a persistent newline-delimited JSON tool server. It keeps
human-readable logs on stderr and emits exactly one JSON response on stdout for
each request line. Run it from the repository root and keep stdin open while
jobs are active.

Use one harness process per harness root. The server confines imported source
scenes to the repository workspace and does not accept an executable or shell
command from a tool request. FFmpeg is resolved from the server process's
operator-controlled `PATH`.

```bash
cargo run --release -p fractal-renderer-harness -- \
  --root output/harness \
  --max-gpu-duty-cycle 80 \
  --max-concurrent-jobs 2
```

Every request has the following envelope:

```json
{
  "id": "request-1",
  "idempotency_key": "alchemy-project-v1",
  "tool": "project.create",
  "arguments": {}
}
```

`id` is only correlated back to the caller. Reusing an `idempotency_key` with
the identical tool and arguments returns the persisted response without
repeating the mutation or starting another job. Reusing it with different
arguments is rejected.

Use `capabilities.describe` to obtain all tool names and their JSON input
schemas:

```json
{"id":1,"tool":"capabilities.describe","arguments":{}}
```

## Alchemy milestone workflow

Import the existing scene. Source scenes are read-only inputs; the harness
stores its own immutable revision.

```json
{"id":2,"idempotency_key":"create-alchemy","tool":"project.create","arguments":{"project_id":"alchemy","scene_path":"scenes/examples/alchemy-pseudo-kleinian-target-orbit.yaml"}}
```

Use the returned `current_revision` as `base_revision`. This example changes
the orbit to 45 degrees. All operations are applied and validated as one
transaction.

```json
{"id":3,"idempotency_key":"alchemy-orbit-45","tool":"scene.patch","arguments":{"project_id":"alchemy","base_revision":"rev-...","operations":[{"op":"set","path":"animation.path.parameters.revolutions","value":0.125}]}}
```

Inspect representative camera samples and 121 evenly spaced DE-clearance
samples without using the GPU:

```json
{"id":4,"tool":"route.inspect","arguments":{"project_id":"alchemy","revision_id":"rev-...","validation_samples":121,"minimum_clearance_ratio":0.0001}}
```

Surface candidates can be requested independently of route selection:

```json
{"id":5,"tool":"target.search","arguments":{"project_id":"alchemy","revision_id":"rev-...","candidate_count":5,"mode":"best"}}
```

Start the representative five-frame composition preview. `preview.start`
returns immediately with a `job_id`.

```json
{"id":6,"idempotency_key":"alchemy-preview-rev-x","tool":"preview.start","arguments":{"project_id":"alchemy","revision_id":"rev-...","profile":"composition","gpu_duty_cycle":80}}
```

Wait for completion and then compare one or more completed preview jobs:

```json
{"id":7,"tool":"job.wait","arguments":{"project_id":"alchemy","job_id":"job-...","timeout_ms":60000}}
{"id":8,"tool":"preview.compare","arguments":{"project_id":"alchemy","preview_job_ids":["job-...","job-..."]}}
```

The preview result contains per-frame luminance, clipping, contrast, and edge
metrics plus each PNG, a contact sheet, and a metrics manifest. After choosing
a candidate, make it current:

```json
{"id":9,"idempotency_key":"promote-alchemy-rev-x","tool":"scene.promote","arguments":{"project_id":"alchemy","revision_id":"rev-...","expected_current_revision":"rev-base..."}}
```

Start the final sequence with an 80% average GPU duty-cycle cap:

```json
{"id":10,"idempotency_key":"render-alchemy-rev-x","tool":"render.start","arguments":{"project_id":"alchemy","revision_id":"rev-...","gpu_duty_cycle":80}}
```

Long work can be controlled at frame boundaries:

```json
{"id":11,"tool":"job.status","arguments":{"project_id":"alchemy","job_id":"job-..."}}
{"id":12,"tool":"job.pause","arguments":{"project_id":"alchemy","job_id":"job-..."}}
{"id":13,"tool":"job.resume","arguments":{"project_id":"alchemy","job_id":"job-..."}}
{"id":14,"tool":"job.cancel","arguments":{"project_id":"alchemy","job_id":"job-..."}}
```

After a process restart, an unfinished job is reported as `interrupted`.
Restart it against its existing revision-bound frame directory:

```json
{"id":15,"idempotency_key":"resume-alchemy-1","tool":"render.start","arguments":{"project_id":"alchemy","revision_id":"rev-...","resume_source_job_id":"job-...","gpu_duty_cycle":80}}
```

The sequence manifest contains the exact revision and scene hash. Resume is
rejected if either differs, so frames from different revisions cannot mix.

Encode all frames after completion:

```json
{"id":16,"idempotency_key":"encode-alchemy-complete","tool":"encode.start","arguments":{"project_id":"alchemy","source_job_id":"job-...","selection":{"kind":"complete"}}}
```

Or explicitly encode only the contiguous frames currently available from
frame zero:

```json
{"id":17,"idempotency_key":"encode-alchemy-partial-1","tool":"encode.start","arguments":{"project_id":"alchemy","source_job_id":"job-...","selection":{"kind":"available","start_frame":0},"output_name":"alchemy-preview-partial.mp4"}}
```

Explicit bounded ranges use `{"kind":"range","start_frame":120,
"end_frame":359}`. PNG sources are preserved after encoding or failure.

## State and artifacts

The harness root contains:

```text
projects/<project>/
├── project.json
├── revisions/
│   ├── rev-<hash>.scene.yaml
│   └── rev-<hash>.json
└── runs/<job>/
    ├── manifest.json
    ├── preview/
    ├── comparison/
    ├── render/frames/
    └── encode/
```

Artifacts are typed and content-addressed. Job and sequence manifests retain
the project, revision, scene hash, software version, GPU adapter, backend,
resource budget, progress, structured result, warnings, and terminal error.

The current pause granularity is one completed frame. GPU duty-cycle pacing
still operates within a frame, but cancellation does not discard a frame while
its GPU submissions are in progress.
