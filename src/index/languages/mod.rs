pub mod bash;
pub mod c_lang;
pub mod caddyfile;
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
pub mod swift;
pub mod systemd;
pub mod toml_lang;
pub mod typescript;
pub mod yaml;
pub mod zig;

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

const ALL_LANGUAGES: [&dyn LanguageSupport; 37] = [
    &typescript::TypeScriptSupport,
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
