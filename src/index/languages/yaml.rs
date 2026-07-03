use tree_sitter::{Language, Node};

use super::LanguageSupport;
use super::common::node_text;
use crate::index::symbols::{
    ExtractedImport, ExtractedReference, ExtractedSymbol, ParseResult, ReferenceKind, SymbolKind,
};

pub struct YamlSupport;

impl LanguageSupport for YamlSupport {
    fn extensions(&self) -> &[&str] {
        &["yaml", "yml"]
    }

    fn language_name(&self) -> &str {
        "yaml"
    }

    fn tree_sitter_language(&self, _ext: &str) -> Language {
        Language::new(tree_sitter_yaml::LANGUAGE)
    }

    fn extract(&self, source: &[u8], tree: &tree_sitter::Tree) -> ParseResult {
        let mut symbols = Vec::new();
        let mut imports = Vec::new();
        let mut references = Vec::new();
        let root = tree.root_node();

        for i in 0..root.child_count() as u32 {
            if let Some(doc) = root.child(i)
                && doc.kind() == "document"
            {
                extract_document(doc, source, &mut symbols, &mut imports, &mut references);
            }
        }

        ParseResult {
            symbols,
            imports,
            references,
            ..Default::default()
        }
    }
}

/// Detect the YAML format by inspecting top-level keys and dispatch to the
/// appropriate extractor.
fn extract_document(
    doc: Node,
    source: &[u8],
    symbols: &mut Vec<ExtractedSymbol>,
    imports: &mut Vec<ExtractedImport>,
    references: &mut Vec<ExtractedReference>,
) {
    let mapping = match find_top_mapping(doc) {
        Some(m) => m,
        None => return,
    };

    let top_keys = collect_top_keys(mapping, source);

    // GitHub Actions: has `on` (or `true` which is how YAML parses bare `on`)
    // and `jobs`
    if (keys_contain(&top_keys, "on") || keys_contain(&top_keys, "true"))
        && keys_contain(&top_keys, "jobs")
    {
        extract_github_actions(mapping, doc, source, symbols, references);
        return;
    }

    // GitLab CI: has `stages` or job-like top-level keys with `script`
    if keys_contain(&top_keys, "stages") || has_gitlab_job_pattern(mapping, source) {
        extract_gitlab_ci(mapping, doc, source, symbols, imports, references);
        return;
    }

    // docker-compose: has `services` and typically `version` or nested
    // service defs with `image`/`build`
    if keys_contain(&top_keys, "services") && has_compose_pattern(mapping, source) {
        extract_docker_compose(mapping, doc, source, symbols, references);
        return;
    }

    // Ansible: has `hosts` and `tasks`
    if keys_contain(&top_keys, "hosts") || keys_contain(&top_keys, "tasks") {
        extract_ansible(mapping, doc, source, symbols);
        return;
    }

    // Helm chart metadata (`Chart.yaml`): local `dependencies` become edges to
    // vendored subcharts. Detected before the Kubernetes branch because a chart
    // file carries no `kind`.
    if looks_like_helm_chart(mapping, &top_keys, source) {
        extract_helm_chart_deps(mapping, doc, source, symbols, imports);
        return;
    }

    // Kustomize overlays/bases (`kustomization.yaml`, components): the backbone
    // of a GitOps dependency graph. `kind: Kustomization`/`Component`, or a
    // kind-less document carrying kustomize-only keys (`resources`, `bases`, ...).
    if looks_like_kustomization(mapping, &top_keys, source) {
        extract_kustomization(mapping, doc, source, symbols, imports);
        return;
    }

    // Kubernetes manifests: has `kind` and `metadata`
    let kind_val = find_scalar_value_in_mapping(mapping, "kind", source);
    let name_val = find_metadata_name(mapping, source);

    if let (Some(kind), Some(name)) = (kind_val, name_val) {
        let resource_name = format!("{kind}/{name}");
        symbols.push(ExtractedSymbol {
            name: resource_name,
            kind: SymbolKind::Class,
            line_start: doc.start_position().row as u32 + 1,
            line_end: doc.end_position().row as u32 + 1,
            signature: Some(format!("kind: {kind}")),
            is_exported: true,
            parent_idx: None,
            unused_excluded: false,
            complexity: None,
            owner_type: None,
        });

        extract_container_images(mapping, source, symbols);
        extract_configmap_secret_refs(mapping, source, symbols);
        if kind == "Service" {
            extract_service_selectors(mapping, source, symbols);
        }
        // ArgoCD App-of-Apps: an Application/ApplicationSet points at the
        // directory that holds the real manifests via `spec.source.path`.
        if kind == "Application" || kind == "ApplicationSet" {
            extract_argocd_sources(mapping, source, imports);
        }
    } else {
        // Generic YAML: extract top-level keys
        extract_top_level_keys(mapping, source, symbols);
    }
}

// ---------------------------------------------------------------------------
// GitHub Actions
// ---------------------------------------------------------------------------

fn extract_github_actions(
    mapping: Node,
    doc: Node,
    source: &[u8],
    symbols: &mut Vec<ExtractedSymbol>,
    references: &mut Vec<ExtractedReference>,
) {
    // Workflow name
    if let Some(name) = find_scalar_value_in_mapping(mapping, "name", source) {
        symbols.push(ExtractedSymbol {
            name,
            kind: SymbolKind::Workflow,
            line_start: doc.start_position().row as u32 + 1,
            line_end: doc.end_position().row as u32 + 1,
            signature: Some("GitHub Actions workflow".to_string()),
            is_exported: true,
            parent_idx: None,
            unused_excluded: false,
            complexity: None,
            owner_type: None,
        });
    }

    // Triggers (on:)
    for pair in iter_mapping_pairs(mapping) {
        if let Some(key) = pair_key_text(pair, source) {
            // YAML parses bare `on:` as boolean `true:`
            if key == "on" || key == "true" {
                if let Some(val) = pair_value_text(pair, source) {
                    symbols.push(ExtractedSymbol {
                        name: format!("on:{val}"),
                        kind: SymbolKind::Variable,
                        line_start: pair.start_position().row as u32 + 1,
                        line_end: pair.end_position().row as u32 + 1,
                        signature: Some(format!("on: {val}")),
                        is_exported: false,
                        parent_idx: None,
                        unused_excluded: false,
                        complexity: None,
                        owner_type: None,
                    });
                } else if let Some(trigger_mapping) = pair
                    .child_by_field_name("value")
                    .and_then(find_block_mapping_recursive)
                {
                    for trigger_pair in iter_mapping_pairs(trigger_mapping) {
                        if let Some(trigger) = pair_key_text(trigger_pair, source) {
                            symbols.push(ExtractedSymbol {
                                name: format!("on:{trigger}"),
                                kind: SymbolKind::Variable,
                                line_start: trigger_pair.start_position().row as u32 + 1,
                                line_end: trigger_pair.end_position().row as u32 + 1,
                                signature: Some(format!("on: {trigger}")),
                                is_exported: false,
                                parent_idx: None,
                                unused_excluded: false,
                                complexity: None,
                                owner_type: None,
                            });
                        }
                    }
                }
            }
        }
    }

    // Jobs
    if let Some(jobs_mapping) = find_value_mapping(mapping, "jobs", source) {
        for job_pair in iter_mapping_pairs(jobs_mapping) {
            if let Some(job_id) = pair_key_text(job_pair, source) {
                let job_name = job_pair
                    .child_by_field_name("value")
                    .and_then(find_block_mapping_recursive)
                    .and_then(|m| find_scalar_value_in_mapping(m, "name", source))
                    .unwrap_or_else(|| job_id.clone());

                symbols.push(ExtractedSymbol {
                    name: job_id.clone(),
                    kind: SymbolKind::Job,
                    line_start: job_pair.start_position().row as u32 + 1,
                    line_end: job_pair.end_position().row as u32 + 1,
                    signature: Some(format!("job: {job_name}")),
                    is_exported: true,
                    parent_idx: None,
                    unused_excluded: false,
                    complexity: None,
                    owner_type: None,
                });

                // Extract `needs:` dependency references
                if let Some(job_mapping) = job_pair
                    .child_by_field_name("value")
                    .and_then(find_block_mapping_recursive)
                {
                    extract_needs_refs(job_mapping, source, references);
                    extract_uses_actions(job_mapping, source, symbols);
                }
            }
        }
    }
}

fn extract_needs_refs(job_mapping: Node, source: &[u8], references: &mut Vec<ExtractedReference>) {
    for pair in iter_mapping_pairs(job_mapping) {
        if let Some(key) = pair_key_text(pair, source)
            && key == "needs"
        {
            if let Some(val) = pair_value_text(pair, source) {
                references.push(ExtractedReference {
                    name: val,
                    line: pair.start_position().row as u32 + 1,
                    from_symbol_idx: None,
                    kind: ReferenceKind::Use,
                    qualifier: None,
                    receiver_type_hint: None,
                    via_method_syntax: false,
                });
            } else if let Some(value_node) = pair.child_by_field_name("value") {
                collect_sequence_values(value_node, source, |val, line| {
                    references.push(ExtractedReference {
                        name: val,
                        line,
                        from_symbol_idx: None,
                        kind: ReferenceKind::Use,
                        qualifier: None,
                        receiver_type_hint: None,
                        via_method_syntax: false,
                    });
                });
            }
        }
    }
}

fn extract_uses_actions(job_mapping: Node, source: &[u8], symbols: &mut Vec<ExtractedSymbol>) {
    if let Some(steps_node) = find_value_node(job_mapping, "steps", source) {
        collect_key_values_recursive(steps_node, "uses", source, symbols);
    }
}

// ---------------------------------------------------------------------------
// GitLab CI
// ---------------------------------------------------------------------------

fn extract_gitlab_ci(
    mapping: Node,
    _doc: Node,
    source: &[u8],
    symbols: &mut Vec<ExtractedSymbol>,
    imports: &mut Vec<ExtractedImport>,
    references: &mut Vec<ExtractedReference>,
) {
    // Extract `stages:` list
    if let Some(stages_node) = find_value_node(mapping, "stages", source) {
        collect_sequence_values(stages_node, source, |val, line| {
            symbols.push(ExtractedSymbol {
                name: format!("stage:{val}"),
                kind: SymbolKind::Variable,
                line_start: line,
                line_end: line,
                signature: Some(format!("stage: {val}")),
                is_exported: false,
                parent_idx: None,
                unused_excluded: false,
                complexity: None,
                owner_type: None,
            });
        });
    }

    // Extract `include:` imports
    if let Some(include_node) = find_value_node(mapping, "include", source) {
        collect_sequence_values(include_node, source, |val, _| {
            imports.push(ExtractedImport {
                source: val,
                specifiers: vec![],
                is_reexport: false,
            });
        });
        if let Some(val) = find_scalar_value_in_mapping(mapping, "include", source) {
            imports.push(ExtractedImport {
                source: val,
                specifiers: vec![],
                is_reexport: false,
            });
        }
    }

    // Reserved GitLab CI keys that are not jobs
    const RESERVED: &[&str] = &[
        "stages",
        "include",
        "variables",
        "default",
        "workflow",
        "image",
        "services",
        "cache",
        "before_script",
        "after_script",
    ];

    for pair in iter_mapping_pairs(mapping) {
        if let Some(key) = pair_key_text(pair, source) {
            if RESERVED.contains(&key.as_str()) {
                continue;
            }

            // Template definitions (starting with dot) are jobs too
            if let Some(job_mapping) = pair
                .child_by_field_name("value")
                .and_then(find_block_mapping_recursive)
            {
                if key.starts_with('.')
                    && !has_key(job_mapping, "script", source)
                    && !has_key(job_mapping, "extends", source)
                {
                    continue;
                }

                symbols.push(ExtractedSymbol {
                    name: key.clone(),
                    kind: SymbolKind::Job,
                    line_start: pair.start_position().row as u32 + 1,
                    line_end: pair.end_position().row as u32 + 1,
                    signature: extract_gitlab_job_sig(&key, job_mapping, source),
                    is_exported: true,
                    parent_idx: None,
                    unused_excluded: false,
                    complexity: None,
                    owner_type: None,
                });

                if let Some(extends_val) =
                    find_scalar_value_in_mapping(job_mapping, "extends", source)
                {
                    references.push(ExtractedReference {
                        name: extends_val,
                        line: pair.start_position().row as u32 + 1,
                        from_symbol_idx: None,
                        kind: ReferenceKind::Use,
                        qualifier: None,
                        receiver_type_hint: None,
                        via_method_syntax: false,
                    });
                }

                extract_needs_refs(job_mapping, source, references);
            }
        }
    }
}

fn extract_gitlab_job_sig(job_name: &str, job_mapping: Node, source: &[u8]) -> Option<String> {
    let stage = find_scalar_value_in_mapping(job_mapping, "stage", source);
    match stage {
        Some(s) => Some(format!("job: {job_name} (stage: {s})")),
        None => Some(format!("job: {job_name}")),
    }
}

fn has_gitlab_job_pattern(mapping: Node, source: &[u8]) -> bool {
    for pair in iter_mapping_pairs(mapping) {
        if let Some(job_mapping) = pair
            .child_by_field_name("value")
            .and_then(find_block_mapping_recursive)
            && (has_key(job_mapping, "script", source) || has_key(job_mapping, "extends", source))
        {
            return true;
        }
    }
    false
}

// ---------------------------------------------------------------------------
// docker-compose
// ---------------------------------------------------------------------------

fn extract_docker_compose(
    mapping: Node,
    _doc: Node,
    source: &[u8],
    symbols: &mut Vec<ExtractedSymbol>,
    references: &mut Vec<ExtractedReference>,
) {
    if let Some(services_mapping) = find_value_mapping(mapping, "services", source) {
        for pair in iter_mapping_pairs(services_mapping) {
            if let Some(svc_name) = pair_key_text(pair, source) {
                let image = pair
                    .child_by_field_name("value")
                    .and_then(find_block_mapping_recursive)
                    .and_then(|m| find_scalar_value_in_mapping(m, "image", source));

                let sig = match &image {
                    Some(img) => format!("service: {svc_name} (image: {img})"),
                    None => format!("service: {svc_name}"),
                };

                symbols.push(ExtractedSymbol {
                    name: svc_name.clone(),
                    kind: SymbolKind::Service,
                    line_start: pair.start_position().row as u32 + 1,
                    line_end: pair.end_position().row as u32 + 1,
                    signature: Some(sig),
                    is_exported: true,
                    parent_idx: None,
                    unused_excluded: false,
                    complexity: None,
                    owner_type: None,
                });

                if let Some(svc_mapping) = pair
                    .child_by_field_name("value")
                    .and_then(find_block_mapping_recursive)
                    && let Some(deps_node) = find_value_node(svc_mapping, "depends_on", source)
                {
                    collect_sequence_values(deps_node, source, |dep, line| {
                        references.push(ExtractedReference {
                            name: dep,
                            line,
                            from_symbol_idx: None,
                            kind: ReferenceKind::Use,
                            qualifier: None,
                            receiver_type_hint: None,
                            via_method_syntax: false,
                        });
                    });
                    if let Some(deps_mapping) = find_block_mapping_recursive(deps_node) {
                        for dep_pair in iter_mapping_pairs(deps_mapping) {
                            if let Some(dep_name) = pair_key_text(dep_pair, source) {
                                references.push(ExtractedReference {
                                    name: dep_name,
                                    line: dep_pair.start_position().row as u32 + 1,
                                    from_symbol_idx: None,
                                    kind: ReferenceKind::Use,
                                    qualifier: None,
                                    receiver_type_hint: None,
                                    via_method_syntax: false,
                                });
                            }
                        }
                    }
                }
            }
        }
    }

    if let Some(volumes_mapping) = find_value_mapping(mapping, "volumes", source) {
        for pair in iter_mapping_pairs(volumes_mapping) {
            if let Some(vol_name) = pair_key_text(pair, source) {
                symbols.push(ExtractedSymbol {
                    name: vol_name,
                    kind: SymbolKind::Volume,
                    line_start: pair.start_position().row as u32 + 1,
                    line_end: pair.end_position().row as u32 + 1,
                    signature: None,
                    is_exported: true,
                    parent_idx: None,
                    unused_excluded: false,
                    complexity: None,
                    owner_type: None,
                });
            }
        }
    }

    if let Some(networks_mapping) = find_value_mapping(mapping, "networks", source) {
        for pair in iter_mapping_pairs(networks_mapping) {
            if let Some(net_name) = pair_key_text(pair, source) {
                symbols.push(ExtractedSymbol {
                    name: net_name,
                    kind: SymbolKind::Network,
                    line_start: pair.start_position().row as u32 + 1,
                    line_end: pair.end_position().row as u32 + 1,
                    signature: None,
                    is_exported: true,
                    parent_idx: None,
                    unused_excluded: false,
                    complexity: None,
                    owner_type: None,
                });
            }
        }
    }
}

fn has_compose_pattern(mapping: Node, source: &[u8]) -> bool {
    if let Some(services_mapping) = find_value_mapping(mapping, "services", source) {
        for pair in iter_mapping_pairs(services_mapping) {
            if let Some(svc_mapping) = pair
                .child_by_field_name("value")
                .and_then(find_block_mapping_recursive)
                && (has_key(svc_mapping, "image", source)
                    || has_key(svc_mapping, "build", source)
                    || has_key(svc_mapping, "container_name", source))
            {
                return true;
            }
        }
    }
    false
}

// ---------------------------------------------------------------------------
// Ansible
// ---------------------------------------------------------------------------

fn extract_ansible(mapping: Node, doc: Node, source: &[u8], symbols: &mut Vec<ExtractedSymbol>) {
    if let Some(play_name) = find_scalar_value_in_mapping(mapping, "name", source) {
        symbols.push(ExtractedSymbol {
            name: play_name,
            kind: SymbolKind::Class,
            line_start: doc.start_position().row as u32 + 1,
            line_end: doc.end_position().row as u32 + 1,
            signature: Some("Ansible play".to_string()),
            is_exported: true,
            parent_idx: None,
            unused_excluded: false,
            complexity: None,
            owner_type: None,
        });
    }

    if let Some(tasks_node) = find_value_node(mapping, "tasks", source) {
        extract_ansible_tasks(tasks_node, source, symbols);
    }

    if let Some(handlers_node) = find_value_node(mapping, "handlers", source) {
        extract_ansible_tasks(handlers_node, source, symbols);
    }

    if let Some(roles_node) = find_value_node(mapping, "roles", source) {
        collect_sequence_values(roles_node, source, |role, line| {
            symbols.push(ExtractedSymbol {
                name: role,
                kind: SymbolKind::Module,
                line_start: line,
                line_end: line,
                signature: Some("role".to_string()),
                is_exported: false,
                parent_idx: None,
                unused_excluded: false,
                complexity: None,
                owner_type: None,
            });
        });
    }
}

fn extract_ansible_tasks(node: Node, source: &[u8], symbols: &mut Vec<ExtractedSymbol>) {
    for child in iter_sequence_items(node) {
        if let Some(task_mapping) = find_block_mapping_recursive(child)
            && let Some(task_name) = find_scalar_value_in_mapping(task_mapping, "name", source)
        {
            symbols.push(ExtractedSymbol {
                name: task_name,
                kind: SymbolKind::Task,
                line_start: child.start_position().row as u32 + 1,
                line_end: child.end_position().row as u32 + 1,
                signature: None,
                is_exported: false,
                parent_idx: None,
                unused_excluded: false,
                complexity: None,
                owner_type: None,
            });
        }
    }
}

// ---------------------------------------------------------------------------
// Kubernetes helpers (preserved from original)
// ---------------------------------------------------------------------------

fn extract_top_level_keys(mapping: Node, source: &[u8], symbols: &mut Vec<ExtractedSymbol>) {
    for pair in iter_mapping_pairs(mapping) {
        if let Some(key) = pair_key_text(pair, source) {
            symbols.push(ExtractedSymbol {
                name: key,
                kind: SymbolKind::Variable,
                line_start: pair.start_position().row as u32 + 1,
                line_end: pair.end_position().row as u32 + 1,
                signature: None,
                is_exported: true,
                parent_idx: None,
                unused_excluded: false,
                complexity: None,
                owner_type: None,
            });
        }
    }
}

fn extract_container_images(root_mapping: Node, source: &[u8], symbols: &mut Vec<ExtractedSymbol>) {
    collect_key_values_recursive(root_mapping, "image", source, symbols);
}

fn collect_key_values_recursive(
    node: Node,
    target_key: &str,
    source: &[u8],
    symbols: &mut Vec<ExtractedSymbol>,
) {
    if node.kind() == "block_mapping_pair"
        && let Some(key) = pair_key_text(node, source)
        && key == target_key
    {
        if let Some(val) = pair_value_text(node, source) {
            symbols.push(ExtractedSymbol {
                name: val.clone(),
                kind: SymbolKind::Variable,
                line_start: node.start_position().row as u32 + 1,
                line_end: node.end_position().row as u32 + 1,
                signature: Some(format!("{target_key}: {val}")),
                is_exported: false,
                parent_idx: None,
                unused_excluded: false,
                complexity: None,
                owner_type: None,
            });
        }
        return;
    }
    for i in 0..node.child_count() as u32 {
        if let Some(child) = node.child(i) {
            collect_key_values_recursive(child, target_key, source, symbols);
        }
    }
}

fn extract_configmap_secret_refs(
    root_mapping: Node,
    source: &[u8],
    symbols: &mut Vec<ExtractedSymbol>,
) {
    let mut refs = Vec::new();
    collect_refs_recursive(root_mapping, source, &mut refs);
    for (ref_kind, ref_name, line_start, line_end) in refs {
        symbols.push(ExtractedSymbol {
            name: format!("{ref_kind}/{ref_name}"),
            kind: SymbolKind::Variable,
            line_start,
            line_end,
            signature: Some(format!("{ref_kind}: {ref_name}")),
            is_exported: false,
            parent_idx: None,
            unused_excluded: false,
            complexity: None,
            owner_type: None,
        });
    }
}

fn collect_refs_recursive(node: Node, source: &[u8], refs: &mut Vec<(String, String, u32, u32)>) {
    if node.kind() == "block_mapping_pair"
        && let Some(key) = pair_key_text(node, source)
    {
        let (ref_kind, found) = match key.as_str() {
            "configMapRef" | "configMapKeyRef" => ("ConfigMap", true),
            "secretRef" | "secretKeyRef" => ("Secret", true),
            "configMap" => {
                if let Some(name) = find_name_in_pair_value(node, source) {
                    refs.push((
                        "ConfigMap".to_string(),
                        name,
                        node.start_position().row as u32 + 1,
                        node.end_position().row as u32 + 1,
                    ));
                }
                ("", false)
            }
            "secret" => {
                if let Some(name) = find_name_in_pair_value(node, source) {
                    refs.push((
                        "Secret".to_string(),
                        name,
                        node.start_position().row as u32 + 1,
                        node.end_position().row as u32 + 1,
                    ));
                }
                ("", false)
            }
            _ => ("", false),
        };
        if found {
            if let Some(name) = find_name_in_pair_value(node, source) {
                refs.push((
                    ref_kind.to_string(),
                    name,
                    node.start_position().row as u32 + 1,
                    node.end_position().row as u32 + 1,
                ));
            }
            return;
        }
    }
    for i in 0..node.child_count() as u32 {
        if let Some(child) = node.child(i) {
            collect_refs_recursive(child, source, refs);
        }
    }
}

fn extract_service_selectors(
    root_mapping: Node,
    source: &[u8],
    symbols: &mut Vec<ExtractedSymbol>,
) {
    let spec_mapping = match find_value_mapping(root_mapping, "spec", source) {
        Some(m) => m,
        None => return,
    };
    let selector_mapping = match find_value_mapping(spec_mapping, "selector", source) {
        Some(m) => m,
        None => return,
    };
    for pair in iter_mapping_pairs(selector_mapping) {
        if let Some(key) = pair_key_text(pair, source)
            && let Some(val) = pair_value_text(pair, source)
        {
            symbols.push(ExtractedSymbol {
                name: format!("selector:{key}={val}"),
                kind: SymbolKind::Variable,
                line_start: pair.start_position().row as u32 + 1,
                line_end: pair.end_position().row as u32 + 1,
                signature: Some(format!("selector: {key}={val}")),
                is_exported: false,
                parent_idx: None,
                unused_excluded: false,
                complexity: None,
                owner_type: None,
            });
        }
    }
}

// ---------------------------------------------------------------------------
// YAML tree-sitter helpers
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Kustomize / Helm / ArgoCD (infra dependency edges)
// ---------------------------------------------------------------------------

/// Whether `raw` is a remote reference (git/http/oci or a bare host path) that
/// has no local file to link, as opposed to a repo-relative path.
fn is_remote_ref(raw: &str) -> bool {
    let v = raw.trim();
    v.contains("://")
        || v.starts_with("git@")
        || v.contains("?ref=")
        || v.starts_with("github.com")
        || v.starts_with("gitlab.com")
        || v.starts_with("bitbucket.org")
}

/// Record a local path reference as an import edge, skipping empties and remote
/// references. Path resolution (bare siblings, `dir → kustomization.yaml`,
/// terraform module dirs) happens later in the index resolver.
fn push_local_import(raw: &str, imports: &mut Vec<ExtractedImport>) {
    let val = raw.trim();
    if val.is_empty() || val == "." || is_remote_ref(val) {
        return;
    }
    imports.push(ExtractedImport {
        source: val.to_string(),
        specifiers: Vec::new(),
        is_reexport: false,
    });
}

/// Kustomize keys distinctive enough that their mere presence identifies a
/// kustomization, even without a `kind`. Deliberately excludes `resources`,
/// which a Helm `values.yaml` also uses (as a mapping) for container
/// requests/limits.
const KUSTOMIZE_DISTINCTIVE_KEYS: &[&str] = &[
    "bases",
    "components",
    "crds",
    "configurations",
    "patchesStrategicMerge",
    "patchesJson6902",
    "configMapGenerator",
    "secretGenerator",
    "helmCharts",
    "generators",
    "transformers",
];

/// Whether `key`'s value in `mapping` is a YAML sequence (list), as opposed to a
/// mapping or scalar. Used to tell a Kustomize `resources:` (a list of paths)
/// apart from a Helm values `resources:` (a requests/limits mapping).
fn key_value_is_sequence(mapping: Node, key: &str, source: &[u8]) -> bool {
    find_value_node(mapping, key, source)
        .is_some_and(|value| iter_sequence_items(value).next().is_some())
}

fn looks_like_kustomization(mapping: Node, top_keys: &[String], source: &[u8]) -> bool {
    match find_scalar_value_in_mapping(mapping, "kind", source).as_deref() {
        Some("Kustomization") | Some("Component") => true,
        // Any other explicit `kind` is a regular manifest, not a kustomization.
        Some(_) => false,
        // Kind-less: a distinctive kustomize key, or `resources`/`bases`/
        // `components` present as an actual path sequence.
        None => {
            KUSTOMIZE_DISTINCTIVE_KEYS
                .iter()
                .any(|k| keys_contain(top_keys, k))
                || key_value_is_sequence(mapping, "resources", source)
                || key_value_is_sequence(mapping, "bases", source)
                || key_value_is_sequence(mapping, "components", source)
        }
    }
}

fn looks_like_helm_chart(mapping: Node, top_keys: &[String], source: &[u8]) -> bool {
    keys_contain(top_keys, "dependencies")
        && keys_contain(top_keys, "name")
        && (keys_contain(top_keys, "version") || keys_contain(top_keys, "apiVersion"))
        && find_scalar_value_in_mapping(mapping, "kind", source).is_none()
}

/// Emit one symbol for the kustomization plus an import edge for every local
/// path it references (resources, bases, components, patches, generator files,
/// helm values files).
fn extract_kustomization(
    doc_mapping: Node,
    doc: Node,
    source: &[u8],
    symbols: &mut Vec<ExtractedSymbol>,
    imports: &mut Vec<ExtractedImport>,
) {
    symbols.push(ExtractedSymbol {
        name: "Kustomization".to_string(),
        kind: SymbolKind::Module,
        line_start: doc.start_position().row as u32 + 1,
        line_end: doc.end_position().row as u32 + 1,
        signature: Some("kustomization".to_string()),
        is_exported: true,
        parent_idx: None,
        unused_excluded: false,
        complexity: None,
        owner_type: None,
    });

    // Plain path sequences: each item is a file or directory reference.
    for key in [
        "resources",
        "bases",
        "components",
        "crds",
        "configurations",
        "patchesStrategicMerge",
    ] {
        if let Some(seq) = find_value_node(doc_mapping, key, source) {
            collect_sequence_values(seq, source, |val, _| push_local_import(&val, imports));
        }
    }

    // Structured patch lists: each item is a mapping with a `path`.
    for key in ["patches", "patchesJson6902"] {
        if let Some(seq) = find_value_node(doc_mapping, key, source) {
            for item in iter_sequence_items(seq) {
                if let Some(m) = find_block_mapping_recursive(item)
                    && let Some(path) = find_scalar_value_in_mapping(m, "path", source)
                {
                    push_local_import(&path, imports);
                }
            }
        }
    }

    // Generators: `files:`/`envs:` entries may be `path` or `key=path`.
    for key in ["configMapGenerator", "secretGenerator"] {
        if let Some(seq) = find_value_node(doc_mapping, key, source) {
            for item in iter_sequence_items(seq) {
                let Some(m) = find_block_mapping_recursive(item) else {
                    continue;
                };
                for field in ["files", "envs"] {
                    if let Some(fseq) = find_value_node(m, field, source) {
                        collect_sequence_values(fseq, source, |val, _| {
                            let path = val.rsplit('=').next().unwrap_or(&val);
                            push_local_import(path, imports);
                        });
                    }
                }
            }
        }
    }

    // Inflated Helm charts: a local values override file.
    if let Some(seq) = find_value_node(doc_mapping, "helmCharts", source) {
        for item in iter_sequence_items(seq) {
            if let Some(m) = find_block_mapping_recursive(item)
                && let Some(vf) = find_scalar_value_in_mapping(m, "valuesFile", source)
            {
                push_local_import(&vf, imports);
            }
        }
    }
}

/// Emit an import edge for each local Helm chart dependency: a `file://` path or
/// a vendored `charts/<name>` subchart.
fn extract_helm_chart_deps(
    doc_mapping: Node,
    doc: Node,
    source: &[u8],
    symbols: &mut Vec<ExtractedSymbol>,
    imports: &mut Vec<ExtractedImport>,
) {
    if let Some(name) = find_scalar_value_in_mapping(doc_mapping, "name", source) {
        symbols.push(ExtractedSymbol {
            name,
            kind: SymbolKind::Module,
            line_start: doc.start_position().row as u32 + 1,
            line_end: doc.end_position().row as u32 + 1,
            signature: Some("helm chart".to_string()),
            is_exported: true,
            parent_idx: None,
            unused_excluded: false,
            complexity: None,
            owner_type: None,
        });
    }

    let Some(deps) = find_value_node(doc_mapping, "dependencies", source) else {
        return;
    };
    for item in iter_sequence_items(deps) {
        let Some(m) = find_block_mapping_recursive(item) else {
            continue;
        };
        if let Some(repo) = find_scalar_value_in_mapping(m, "repository", source)
            && let Some(local) = repo.strip_prefix("file://")
        {
            push_local_import(local, imports);
        }
        if let Some(name) = find_scalar_value_in_mapping(m, "name", source) {
            // Convention: a vendored subchart lives under `charts/<name>`.
            push_local_import(&format!("charts/{name}"), imports);
        }
    }
}

/// Emit an import edge for each `spec.source.path` an ArgoCD Application (or the
/// `spec.template.spec` of an ApplicationSet) points at. Paths are repo-root
/// relative, which the resolver handles.
fn extract_argocd_sources(doc_mapping: Node, source: &[u8], imports: &mut Vec<ExtractedImport>) {
    let mut spec_nodes: Vec<Node> = Vec::new();
    if let Some(spec) = find_value_mapping(doc_mapping, "spec", source) {
        spec_nodes.push(spec);
        // ApplicationSet nests the real source under spec.template.spec.
        if let Some(template) = find_value_mapping(spec, "template", source)
            && let Some(tmpl_spec) = find_value_mapping(template, "spec", source)
        {
            spec_nodes.push(tmpl_spec);
        }
    }

    for spec in spec_nodes {
        if let Some(src) = find_value_mapping(spec, "source", source)
            && let Some(path) = find_scalar_value_in_mapping(src, "path", source)
        {
            push_local_import(&path, imports);
        }
        if let Some(seq) = find_value_node(spec, "sources", source) {
            for item in iter_sequence_items(seq) {
                if let Some(m) = find_block_mapping_recursive(item)
                    && let Some(path) = find_scalar_value_in_mapping(m, "path", source)
                {
                    push_local_import(&path, imports);
                }
            }
        }
    }
}

fn collect_top_keys(mapping: Node, source: &[u8]) -> Vec<String> {
    iter_mapping_pairs(mapping)
        .filter_map(|pair| pair_key_text(pair, source))
        .collect()
}

fn keys_contain(keys: &[String], needle: &str) -> bool {
    keys.iter().any(|k| k == needle)
}

fn has_key(mapping: Node, key: &str, source: &[u8]) -> bool {
    iter_mapping_pairs(mapping).any(|pair| pair_key_text(pair, source).is_some_and(|k| k == key))
}

fn find_top_mapping(doc: Node) -> Option<Node> {
    find_child_by_kind(doc, "block_node")
        .and_then(|n| find_child_by_kind(n, "block_mapping"))
        .or_else(|| find_child_by_kind(doc, "block_mapping"))
}

fn find_child_by_kind<'a>(node: Node<'a>, kind: &str) -> Option<Node<'a>> {
    for i in 0..node.child_count() as u32 {
        if let Some(child) = node.child(i)
            && child.kind() == kind
        {
            return Some(child);
        }
    }
    None
}

fn iter_mapping_pairs(mapping: Node) -> impl Iterator<Item = Node> {
    (0..mapping.child_count() as u32)
        .filter_map(move |i| mapping.child(i))
        .filter(|n| n.kind() == "block_mapping_pair")
}

fn iter_sequence_items(node: Node) -> impl Iterator<Item = Node> {
    let mut items = Vec::new();
    collect_sequence_items(node, &mut items);
    items.into_iter()
}

fn collect_sequence_items<'a>(node: Node<'a>, items: &mut Vec<Node<'a>>) {
    match node.kind() {
        "block_sequence" => {
            for i in 0..node.child_count() as u32 {
                if let Some(child) = node.child(i)
                    && child.kind() == "block_sequence_item"
                {
                    items.push(child);
                }
            }
        }
        "flow_sequence" => {
            // Flow sequences: [a, b, c]
            for i in 0..node.child_count() as u32 {
                if let Some(child) = node.child(i) {
                    match child.kind() {
                        // Skip brackets and commas
                        "[" | "]" | "," => {}
                        _ => items.push(child),
                    }
                }
            }
        }
        "block_node" | "flow_node" => {
            for i in 0..node.child_count() as u32 {
                if let Some(child) = node.child(i) {
                    collect_sequence_items(child, items);
                }
            }
        }
        _ => {}
    }
}

fn collect_sequence_values(node: Node, source: &[u8], mut callback: impl FnMut(String, u32)) {
    for item in iter_sequence_items(node) {
        if let Some(text) = scalar_text_recursive(item, source) {
            callback(text, item.start_position().row as u32 + 1);
        }
    }
}

fn scalar_text_recursive(node: Node, source: &[u8]) -> Option<String> {
    if let Some(text) = scalar_text(node, source) {
        return Some(text);
    }
    for i in 0..node.child_count() as u32 {
        if let Some(child) = node.child(i)
            && let Some(text) = scalar_text_recursive(child, source)
        {
            return Some(text);
        }
    }
    None
}

fn find_value_mapping<'a>(mapping: Node<'a>, key: &str, source: &[u8]) -> Option<Node<'a>> {
    for pair in iter_mapping_pairs(mapping) {
        if let Some(k) = pair_key_text(pair, source)
            && k == key
        {
            let value_node = pair.child_by_field_name("value")?;
            return find_block_mapping_recursive(value_node);
        }
    }
    None
}

fn find_value_node<'a>(mapping: Node<'a>, key: &str, source: &[u8]) -> Option<Node<'a>> {
    for pair in iter_mapping_pairs(mapping) {
        if let Some(k) = pair_key_text(pair, source)
            && k == key
        {
            return pair.child_by_field_name("value");
        }
    }
    None
}

fn find_block_mapping_recursive(node: Node) -> Option<Node> {
    if node.kind() == "block_mapping" {
        return Some(node);
    }
    for i in 0..node.child_count() as u32 {
        if let Some(child) = node.child(i)
            && let Some(found) = find_block_mapping_recursive(child)
        {
            return Some(found);
        }
    }
    None
}

fn find_scalar_value_in_mapping(mapping: Node, key: &str, source: &[u8]) -> Option<String> {
    for pair in iter_mapping_pairs(mapping) {
        if let Some(k) = pair_key_text(pair, source)
            && k == key
        {
            return pair_value_text(pair, source);
        }
    }
    None
}

fn find_metadata_name(mapping: Node, source: &[u8]) -> Option<String> {
    let metadata_mapping = find_value_mapping(mapping, "metadata", source)?;
    find_scalar_value_in_mapping(metadata_mapping, "name", source)
}

fn find_name_in_pair_value(pair: Node, source: &[u8]) -> Option<String> {
    let value_node = pair.child_by_field_name("value")?;
    let inner_mapping = find_block_mapping_recursive(value_node)?;
    find_scalar_value_in_mapping(inner_mapping, "name", source)
}

fn pair_key_text(pair: Node, source: &[u8]) -> Option<String> {
    let key_node = pair.child_by_field_name("key")?;
    scalar_text(key_node, source)
}

fn pair_value_text(pair: Node, source: &[u8]) -> Option<String> {
    let value_node = pair.child_by_field_name("value")?;
    scalar_text(value_node, source)
}

fn scalar_text(node: Node, source: &[u8]) -> Option<String> {
    match node.kind() {
        "flow_node" | "block_node" => {
            for i in 0..node.child_count() as u32 {
                if let Some(child) = node.child(i)
                    && let Some(text) = scalar_text(child, source)
                {
                    return Some(text);
                }
            }
            None
        }
        "plain_scalar" | "double_quote_scalar" | "single_quote_scalar" => {
            let text = node_text(node, source);
            let trimmed = text.trim_matches(|c| c == '"' || c == '\'');
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tree_sitter::Parser;

    fn parse_yaml(source: &str) -> ParseResult {
        let mut parser = Parser::new();
        let lang = Language::new(tree_sitter_yaml::LANGUAGE);
        parser.set_language(&lang).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let support = YamlSupport;
        support.extract(source.as_bytes(), &tree)
    }

    // --- Kubernetes tests ---

    #[test]
    fn test_k8s_deployment() {
        let result = parse_yaml(
            r#"apiVersion: apps/v1
kind: Deployment
metadata:
  name: contentforge-api
spec:
  template:
    spec:
      containers:
        - name: api
          image: registry.example.com/contentforge:latest
"#,
        );
        let names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"Deployment/contentforge-api"));
        assert!(names.iter().any(|n| n.contains("registry.example.com")));
    }

    #[test]
    fn test_k8s_service_with_selector() {
        let result = parse_yaml(
            r#"apiVersion: v1
kind: Service
metadata:
  name: my-service
spec:
  selector:
    app: my-app
  ports:
    - port: 80
"#,
        );
        let names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"Service/my-service"));
        assert!(names.contains(&"selector:app=my-app"));
    }

    #[test]
    fn test_k8s_configmap_ref() {
        let result = parse_yaml(
            r#"apiVersion: apps/v1
kind: Deployment
metadata:
  name: my-deploy
spec:
  template:
    spec:
      containers:
        - name: app
          envFrom:
            - configMapRef:
                name: my-config
"#,
        );
        let names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"Deployment/my-deploy"));
        assert!(names.contains(&"ConfigMap/my-config"));
    }

    #[test]
    fn test_k8s_secret_ref() {
        let result = parse_yaml(
            r#"apiVersion: apps/v1
kind: Deployment
metadata:
  name: my-deploy
spec:
  template:
    spec:
      containers:
        - name: app
          envFrom:
            - secretRef:
                name: my-secret
"#,
        );
        let names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"Secret/my-secret"));
    }

    #[test]
    fn test_non_k8s_yaml() {
        let result = parse_yaml(
            r#"database:
  host: localhost
  port: 5432
server:
  bind: 0.0.0.0
"#,
        );
        let names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"database"));
        assert!(names.contains(&"server"));
    }

    #[test]
    fn test_multi_document_yaml() {
        let result = parse_yaml(
            r#"apiVersion: v1
kind: ConfigMap
metadata:
  name: my-config
---
apiVersion: v1
kind: Service
metadata:
  name: my-service
spec:
  selector:
    app: test
"#,
        );
        let names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"ConfigMap/my-config"));
        assert!(names.contains(&"Service/my-service"));
    }

    #[test]
    fn test_volume_configmap_ref() {
        let result = parse_yaml(
            r#"apiVersion: apps/v1
kind: Deployment
metadata:
  name: my-deploy
spec:
  template:
    spec:
      volumes:
        - name: config-vol
          configMap:
            name: my-configmap
"#,
        );
        let names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"ConfigMap/my-configmap"));
    }

    // --- GitHub Actions tests ---

    #[test]
    fn test_github_actions_workflow() {
        let result = parse_yaml(
            r#"name: CI Pipeline
on:
  push:
    branches: [main]
  pull_request:

jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - run: cargo build

  test:
    needs: [build]
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - run: cargo test
"#,
        );
        let names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"CI Pipeline"));
        assert!(names.contains(&"build"));
        assert!(names.contains(&"test"));

        let jobs: Vec<_> = result
            .symbols
            .iter()
            .filter(|s| matches!(s.kind, SymbolKind::Job))
            .collect();
        assert_eq!(jobs.len(), 2);

        assert!(result.references.iter().any(|r| r.name == "build"));
    }

    #[test]
    fn test_github_actions_uses() {
        let result = parse_yaml(
            r#"name: Deploy
on: push

jobs:
  deploy:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: docker/build-push-action@v5
"#,
        );
        let names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.iter().any(|n| n.contains("actions/checkout")));
    }

    // --- GitLab CI tests ---

    #[test]
    fn test_gitlab_ci() {
        let result = parse_yaml(
            r#"stages:
  - build
  - test
  - deploy

build_job:
  stage: build
  script:
    - cargo build

test_job:
  stage: test
  needs: [build_job]
  script:
    - cargo test
"#,
        );
        let names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"build_job"));
        assert!(names.contains(&"test_job"));
        assert!(names.iter().any(|n| n.contains("stage:build")));

        assert!(result.references.iter().any(|r| r.name == "build_job"));
    }

    #[test]
    fn test_gitlab_ci_extends() {
        let result = parse_yaml(
            r#"stages:
  - test

.test_template:
  script:
    - echo "testing"

unit_test:
  extends: .test_template
  stage: test
"#,
        );
        let names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&".test_template"));
        assert!(names.contains(&"unit_test"));

        assert!(result.references.iter().any(|r| r.name == ".test_template"));
    }

    #[test]
    fn test_gitlab_ci_include() {
        let result = parse_yaml(
            r#"include:
  - local: .gitlab/ci/build.yml
  - local: .gitlab/ci/test.yml

stages:
  - build
"#,
        );
        // The include values contain "local:" which is a mapping key, so they
        // won't be extracted as simple strings. The feature still detects the
        // file as GitLab CI and extracts jobs/stages.
        let names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.iter().any(|n| n.contains("stage:build")));
    }

    // --- docker-compose tests ---

    #[test]
    fn test_docker_compose() {
        let result = parse_yaml(
            r#"services:
  web:
    image: nginx:latest
    depends_on:
      - api
      - redis

  api:
    build: ./api
    depends_on:
      - db

  db:
    image: postgres:15

  redis:
    image: redis:7

volumes:
  pgdata:

networks:
  backend:
"#,
        );
        let names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"web"));
        assert!(names.contains(&"api"));
        assert!(names.contains(&"db"));
        assert!(names.contains(&"redis"));
        assert!(names.contains(&"pgdata"));
        assert!(names.contains(&"backend"));

        let services: Vec<_> = result
            .symbols
            .iter()
            .filter(|s| matches!(s.kind, SymbolKind::Service))
            .collect();
        assert_eq!(services.len(), 4);

        assert!(result.references.iter().any(|r| r.name == "api"));
        assert!(result.references.iter().any(|r| r.name == "redis"));
        assert!(result.references.iter().any(|r| r.name == "db"));
    }

    // --- Ansible tests ---

    #[test]
    fn test_ansible_playbook() {
        let result = parse_yaml(
            r#"hosts: webservers
tasks:
  - name: Install nginx
    apt:
      name: nginx
      state: present

  - name: Start nginx
    service:
      name: nginx
      state: started

handlers:
  - name: Restart nginx
    service:
      name: nginx
      state: restarted
"#,
        );
        let names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"Install nginx"));
        assert!(names.contains(&"Start nginx"));
        assert!(names.contains(&"Restart nginx"));

        let tasks: Vec<_> = result
            .symbols
            .iter()
            .filter(|s| matches!(s.kind, SymbolKind::Task))
            .collect();
        assert_eq!(tasks.len(), 3);
    }

    // --- Kustomize / Helm / ArgoCD tests ---

    fn import_sources(result: &ParseResult) -> Vec<&str> {
        result.imports.iter().map(|i| i.source.as_str()).collect()
    }

    #[test]
    fn test_kustomization_resources_and_patches() {
        let result = parse_yaml(
            r#"
apiVersion: kustomize.config.k8s.io/v1beta1
kind: Kustomization
resources:
  - ../../base
  - deployment.yaml
  - service.yaml
components:
  - ../../components/monitoring
patchesStrategicMerge:
  - patch-replicas.yaml
patches:
  - path: patch-env.yaml
    target:
      kind: Deployment
configMapGenerator:
  - name: app-config
    files:
      - config.properties
      - key.txt=secret.txt
"#,
        );
        let imports = import_sources(&result);
        for expected in [
            "../../base",
            "deployment.yaml",
            "service.yaml",
            "../../components/monitoring",
            "patch-replicas.yaml",
            "patch-env.yaml",
            "config.properties",
            "secret.txt",
        ] {
            assert!(
                imports.contains(&expected),
                "kustomization import {expected} missing from {imports:?}"
            );
        }
        assert!(result.symbols.iter().any(|s| s.name == "Kustomization"));
    }

    #[test]
    fn test_kindless_kustomization_detected() {
        // A kustomization.yaml need not carry `kind:`.
        let result = parse_yaml(
            r#"
resources:
  - namespace.yaml
  - ../base
"#,
        );
        let imports = import_sources(&result);
        assert!(imports.contains(&"namespace.yaml"));
        assert!(imports.contains(&"../base"));
    }

    #[test]
    fn test_kustomization_skips_remote_resources() {
        let result = parse_yaml(
            r#"
kind: Kustomization
resources:
  - github.com/org/repo/overlays/prod?ref=v1.2.3
  - https://example.com/manifest.yaml
  - local.yaml
"#,
        );
        let imports = import_sources(&result);
        assert_eq!(imports, vec!["local.yaml"]);
    }

    #[test]
    fn test_argocd_application_source_path() {
        let result = parse_yaml(
            r#"
apiVersion: argoproj.io/v1alpha1
kind: Application
metadata:
  name: my-app
spec:
  source:
    repoURL: https://example.com/repo.git
    path: k8s-apps/prod/my-app
    targetRevision: main
"#,
        );
        let imports = import_sources(&result);
        assert!(imports.contains(&"k8s-apps/prod/my-app"));
        assert!(
            result
                .symbols
                .iter()
                .any(|s| s.name == "Application/my-app")
        );
    }

    #[test]
    fn test_argocd_applicationset_template_path_and_multi_sources() {
        let result = parse_yaml(
            r#"
apiVersion: argoproj.io/v1alpha1
kind: ApplicationSet
metadata:
  name: my-set
spec:
  template:
    spec:
      sources:
        - repoURL: https://example.com/repo.git
          path: k8s-apps/dev/svc-a
        - repoURL: https://example.com/repo.git
          path: k8s-apps/dev/svc-b
"#,
        );
        let imports = import_sources(&result);
        assert!(imports.contains(&"k8s-apps/dev/svc-a"));
        assert!(imports.contains(&"k8s-apps/dev/svc-b"));
    }

    #[test]
    fn test_helm_chart_dependencies() {
        let result = parse_yaml(
            r#"
apiVersion: v2
name: my-wrapper
version: 0.1.0
dependencies:
  - name: common
    version: 1.0.0
    repository: file://../common
  - name: redis
    version: 17.0.0
    repository: https://charts.bitnami.com/bitnami
"#,
        );
        let imports = import_sources(&result);
        // Local file:// dependency resolves to its path.
        assert!(imports.contains(&"../common"));
        // Vendored subchart convention for each dep name.
        assert!(imports.contains(&"charts/common"));
        assert!(imports.contains(&"charts/redis"));
        // Remote https repository is not a local edge.
        assert!(!imports.iter().any(|i| i.contains("bitnami.com")));
        assert!(result.symbols.iter().any(|s| s.name == "my-wrapper"));
    }

    #[test]
    fn test_helm_values_with_resources_map_not_kustomization() {
        // A Helm values.yaml commonly has a top-level `resources:` MAPPING
        // (container requests/limits). It must NOT be mistaken for a Kustomize
        // file, whose `resources:` is a SEQUENCE of paths.
        let result = parse_yaml(
            r#"
image:
  repository: nginx
  tag: "1.25"
replicaCount: 2
resources:
  requests:
    cpu: 100m
    memory: 128Mi
  limits:
    cpu: 500m
    memory: 256Mi
"#,
        );
        assert!(
            result.imports.is_empty(),
            "values.yaml must not emit import edges, got {:?}",
            result.imports
        );
        assert!(
            !result.symbols.iter().any(|s| s.name == "Kustomization"),
            "values.yaml must not be classified as a Kustomization"
        );
    }

    #[test]
    fn test_empty_resources_sequence_not_kustomization_symbol_still_safe() {
        // `resources: []` (empty) should not crash and produces no edges.
        let result = parse_yaml("resources: []\n");
        assert!(result.imports.is_empty());
    }

    #[test]
    fn test_plain_manifest_not_treated_as_kustomize() {
        // A ClusterRole has nested `resources:` but a real `kind` - must stay a
        // Kubernetes manifest, not a kustomization.
        let result = parse_yaml(
            r#"
apiVersion: rbac.authorization.k8s.io/v1
kind: ClusterRole
metadata:
  name: reader
rules:
  - apiGroups: [""]
    resources: ["pods", "services"]
    verbs: ["get", "list"]
"#,
        );
        assert!(result.imports.is_empty());
        assert!(
            result
                .symbols
                .iter()
                .any(|s| s.name == "ClusterRole/reader")
        );
    }
}
