# Doctrine: Infrastructure / DevOps

Goal: Treat infrastructure-as-code repos (Kubernetes, Kustomize, Helm, Terraform/OpenTofu, ArgoCD/GitOps, Docker, Ansible, CI) as first-class qartez targets. A DevOps task is qartez-first exactly like an app-code task - do not drop to raw `grep`/`find` because the files are YAML or HCL.

## When this applies

Any task in an infra/GitOps repo: "deploy/patch app X", "add an overlay", "bump an image", "wire this manifest into ArgoCD", "which base does this env inherit", "what breaks if I change this module/base", "find the kustomization for service Y".

## What qartez indexes here

- **Kubernetes manifests** - one symbol per resource (`Kind/name`), plus container images and ConfigMap/Secret refs.
- **Kustomize** (`kustomization.yaml`, components) - edges from `resources`, `bases`, `components`, `crds`, `patches`/`patchesStrategicMerge`, `configMapGenerator`/`secretGenerator` files, and `helmCharts` valuesFile.
- **Terraform / OpenTofu** - `resource`/`data`/`module`/`variable`/`output`/`provider`/`locals` symbols; local `module { source = "../x" }` edges resolve to the module's `.tf` files.
- **Helm** - `Chart.yaml` `dependencies` edges (local `file://` and `charts/<name>` subcharts); template symbols.
- **ArgoCD** - `Application`/`ApplicationSet` `spec.source.path` (and `spec.sources[].path`) edges to the app directory - this is the App-of-Apps graph.

## Sequence

1. `qartez_map` - manifest / module layout ranked by importance.
2. `qartez_find` / `qartez_grep` - jump to a resource, module, or `kustomization.yaml` by name instead of globbing.
3. `qartez_deps` - which overlays, bases, components, and modules a file pulls in (and what pulls it in).
4. `qartez_impact` - **before** editing a shared base, component, or Terraform module: see the blast radius across environments.
5. Edit (built-in `Edit` for manifest values; do the required built-in `Read` of the target range first).
6. `qartez_diff_impact` - confirm the change's reach before merge.

## Output standard

- Cite exact file paths (e.g. `k8s-apps/prod/foo/kustomization.yaml`).
- Name the environments/overlays a shared base fans out to when reporting impact.
- Separate exact graph findings (edges qartez resolved) from inferred conclusions (remote bases, runtime templating qartez cannot follow).
- Flag remote references (`github.com/...?ref=`, `oci://`, `https://`) as unresolved by design - they are not local edges.
