pub mod bash;
pub mod c_lang;
pub mod caddyfile;
mod common;
pub mod cpp;
pub mod csharp;
pub mod css;
pub mod dart;
pub mod dockerfile;
pub mod elixir;
pub mod go;
pub mod haskell;
pub mod hcl;
pub mod helm;
pub mod java;
pub mod jenkinsfile;
pub mod jsonnet;
pub mod kotlin;
pub mod lua;
pub mod makefile;
pub mod nginx;
pub mod nix;
pub mod ocaml;
pub mod php;
pub mod protobuf;
pub mod python;
pub mod r;
pub mod ruby;
pub mod rust_lang;
pub mod scala;
pub mod sql;
pub mod starlark;
pub mod svelte;
pub mod swift;
pub mod systemd;
pub mod toml_lang;
pub mod typescript;
pub mod yaml;
pub mod zig;

use std::path::Path;
use std::sync::LazyLock;
use tree_sitter::Language;

use crate::index::symbols::ParseResult;

pub trait LanguageSupport: Send + Sync {
    fn extensions(&self) -> &[&str];
    /// Exact filenames (no extension) that this parser handles, e.g.
    /// `["Dockerfile", "Makefile"]`. Default is empty.
    fn filenames(&self) -> &[&str] {
        &[]
    }
    /// Filename prefixes for matching files like `Dockerfile.prod`.
    /// Default is empty.
    fn filename_prefixes(&self) -> &[&str] {
        &[]
    }
    fn language_name(&self) -> &str;
    fn tree_sitter_language(&self, ext: &str) -> Language;
    fn extract(&self, source: &[u8], tree: &tree_sitter::Tree) -> ParseResult;

    /// Opt-in escape hatch for files that hold more than one language, where
    /// the single `tree_sitter_language` + `extract` path cannot describe the
    /// file (a `.svelte` component is a script block, a style block, and
    /// markup at once). Returning `Some` makes the parser skip that path and
    /// hand the whole file to the language, which parses each embedded region
    /// with the grammar it needs.
    ///
    /// Default `None` keeps every single-grammar language on the normal path.
    fn parse_and_extract(&self, _path: &Path, _source: &[u8]) -> Option<ParseResult> {
        None
    }
}

struct LangEntry {
    ext: &'static str,
    support: &'static dyn LanguageSupport,
}

struct FilenameEntry {
    name: &'static str,
    support: &'static dyn LanguageSupport,
}

struct PrefixEntry {
    prefix: &'static str,
    support: &'static dyn LanguageSupport,
}

static REGISTRY: LazyLock<Vec<LangEntry>> = LazyLock::new(|| {
    let mut entries = Vec::new();
    for lang in &ALL_LANGUAGES {
        for ext in lang.extensions() {
            entries.push(LangEntry {
                ext,
                support: *lang,
            });
        }
    }
    entries
});

static FILENAME_REGISTRY: LazyLock<Vec<FilenameEntry>> = LazyLock::new(|| {
    let mut entries = Vec::new();
    for lang in &ALL_LANGUAGES {
        for name in lang.filenames() {
            entries.push(FilenameEntry {
                name,
                support: *lang,
            });
        }
    }
    entries
});

static PREFIX_REGISTRY: LazyLock<Vec<PrefixEntry>> = LazyLock::new(|| {
    let mut entries = Vec::new();
    for lang in &ALL_LANGUAGES {
        for prefix in lang.filename_prefixes() {
            entries.push(PrefixEntry {
                prefix,
                support: *lang,
            });
        }
    }
    entries
});

const ALL_LANGUAGES: [&dyn LanguageSupport; 38] = [
    &typescript::TypeScriptSupport,
    &svelte::SvelteSupport,
    &rust_lang::RustSupport,
    &go::GoSupport,
    &python::PythonSupport,
    &yaml::YamlSupport,
    &hcl::HclSupport,
    &c_lang::CSupport,
    &cpp::CppSupport,
    &java::JavaSupport,
    &ruby::RubySupport,
    &bash::BashSupport,
    &css::CssSupport,
    &kotlin::KotlinSupport,
    &swift::SwiftSupport,
    &csharp::CSharpSupport,
    &dart::DartSupport,
    &php::PhpSupport,
    &dockerfile::DockerfileSupport,
    &makefile::MakefileSupport,
    &toml_lang::TomlSupport,
    &nginx::NginxSupport,
    &helm::HelmSupport,
    &sql::SqlSupport,
    &protobuf::ProtobufSupport,
    &lua::LuaSupport,
    &scala::ScalaSupport,
    &nix::NixSupport,
    &starlark::StarlarkSupport,
    &jsonnet::JsonnetSupport,
    &elixir::ElixirSupport,
    &jenkinsfile::JenkinsfileSupport,
    &caddyfile::CaddyfileSupport,
    &systemd::SystemdSupport,
    &zig::ZigSupport,
    &haskell::HaskellSupport,
    &ocaml::OCamlSupport,
    &r::RSupport,
];

pub fn get_language_for_ext(ext: &str) -> Option<&'static dyn LanguageSupport> {
    REGISTRY.iter().find(|e| e.ext == ext).map(|e| e.support)
}

pub fn get_language_for_filename(filename: &str) -> Option<&'static dyn LanguageSupport> {
    if let Some(entry) = FILENAME_REGISTRY.iter().find(|e| e.name == filename) {
        return Some(entry.support);
    }
    PREFIX_REGISTRY
        .iter()
        .find(|e| filename.starts_with(e.prefix))
        .map(|e| e.support)
}

pub fn supported_extensions() -> Vec<&'static str> {
    REGISTRY.iter().map(|e| e.ext).collect()
}

pub fn supported_filenames() -> Vec<&'static str> {
    FILENAME_REGISTRY.iter().map(|e| e.name).collect()
}

pub fn supported_prefixes() -> Vec<&'static str> {
    PREFIX_REGISTRY.iter().map(|e| e.prefix).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_language_for_ext_resolves_rust() {
        let lang = get_language_for_ext("rs").expect("rust extension must be registered");
        assert_eq!(lang.language_name(), "rust");
    }

    #[test]
    fn get_language_for_ext_returns_none_for_unknown() {
        assert!(get_language_for_ext("").is_none());
        assert!(get_language_for_ext("xyz_not_a_real_extension").is_none());
    }

    #[test]
    fn get_language_for_ext_is_case_sensitive() {
        assert!(get_language_for_ext("RS").is_none());
    }

    #[test]
    fn get_language_for_filename_resolves_exact_names() {
        let lang = get_language_for_filename("Dockerfile").expect("Dockerfile must be registered");
        assert_eq!(lang.language_name(), "dockerfile");

        let lang = get_language_for_filename("Makefile").expect("Makefile must be registered");
        assert_eq!(lang.language_name(), "makefile");
    }

    #[test]
    fn get_language_for_filename_resolves_by_prefix() {
        let lang =
            get_language_for_filename("Dockerfile.prod").expect("Dockerfile.* prefix must match");
        assert_eq!(lang.language_name(), "dockerfile");

        let lang =
            get_language_for_filename("Jenkinsfile.ci").expect("Jenkinsfile.* prefix must match");
        assert_eq!(lang.language_name(), "jenkinsfile");
    }

    #[test]
    fn get_language_for_filename_returns_none_for_unrelated() {
        assert!(get_language_for_filename("random_file.log").is_none());
        assert!(get_language_for_filename("").is_none());
    }

    #[test]
    fn supported_extensions_contains_core_languages() {
        let exts = supported_extensions();
        for required in ["rs", "py", "go", "ts", "java"] {
            assert!(
                exts.contains(&required),
                "supported_extensions missing {required}"
            );
        }
    }

    #[test]
    fn supported_filenames_contains_known_entries() {
        let names = supported_filenames();
        assert!(names.contains(&"Dockerfile"));
        assert!(names.contains(&"Makefile"));
    }

    #[test]
    fn supported_prefixes_contains_known_entries() {
        let prefixes = supported_prefixes();
        assert!(prefixes.contains(&"Dockerfile."));
        assert!(prefixes.contains(&"Jenkinsfile."));
    }

    #[test]
    fn all_languages_register_non_empty_extensions() {
        for lang in ALL_LANGUAGES.iter() {
            assert!(
                !lang.extensions().is_empty(),
                "language {} registered zero extensions",
                lang.language_name()
            );
        }
    }
}
