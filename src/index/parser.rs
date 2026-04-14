use std::path::Path;
use std::sync::Mutex;

use tree_sitter::{Language, Parser};

use super::languages;
use super::symbols::ParseResult;
use crate::error::{Result, QartezError};

pub struct ParserPool {
    parser: Mutex<Parser>,
}

impl Default for ParserPool {
    fn default() -> Self {
        Self::new()
    }
}

impl ParserPool {
    pub fn new() -> Self {
        Self {
            parser: Mutex::new(Parser::new()),
        }
    }

    pub fn parse_file(&self, path: &Path, source: &[u8]) -> Result<(ParseResult, String)> {
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        let filename = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("");

        let support = languages::get_language_for_ext(ext)
            .or_else(|| languages::get_language_for_filename(filename))
            .ok_or_else(|| QartezError::Parse {
                path: path.display().to_string(),
                message: format!("unsupported file: {filename}"),
            })?;

        let lang: Language = support.tree_sitter_language(ext);
        let mut parser = self.parser.lock().unwrap_or_else(|e| e.into_inner());
        parser
            .set_language(&lang)
            .map_err(|e| QartezError::Parse {
                path: path.display().to_string(),
                message: format!("failed to set language: {e}"),
            })?;

        let tree = parser
            .parse(source, None)
            .ok_or_else(|| QartezError::Parse {
                path: path.display().to_string(),
                message: "tree-sitter parse returned None".to_string(),
            })?;

        let result = support.extract(source, &tree);
        Ok((result, support.language_name().to_string()))
    }
}
