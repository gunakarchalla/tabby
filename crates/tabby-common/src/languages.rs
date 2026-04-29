use std::{collections::HashMap, ffi::OsStr};

use lazy_static::lazy_static;
use serde::{Deserialize, Serialize};

use crate::config;

lazy_static! {
    static ref DEFAULT: Vec<&'static str> = vec![
        "\n\n",
        "\n\n  ",
        "\n\n    ",
        "\n\n      ",
        "\n\n        ",
        "\n\n          ",
        "\n\n            ",
        "\n\n              ",
        "\n\n\t",
        "\n\n\t\t",
        "\n\n\t\t\t",
        "\n\n\t\t\t\t",
        "\n\n\t\t\t\t\t",
        "\n\n\t\t\t\t\t\t",
        "\n\n\t\t\t\t\t\t\t",

        // FIXME: Hack for codellama / codegemma to simplify tabby's implementation.

        // StarCoder
        "<fim_prefix>",
        "<fim_suffix>",
        "<fim_middle>",
        "<file_sep>",

        // CodeLlama
        " <EOT>",

        // CodeGemma
        "<|fim_prefix|>",
        "<|fim_suffix|>",
        "<|fim_middle|>",
        "<|file_separator|>",

        // chat_ml
        "<|system|>",
        "<|user|>",
        "<|end|>",
        "<|assistant|>",
    ];
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct ConfigList {
    config: Vec<Language>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Language {
    languages: Vec<String>,
    exts: Vec<String>,

    top_level_keywords: Option<Vec<String>>,
    pub line_comment: Option<String>,

    #[serde(default)]
    pub block_comment_start: Option<String>,
    #[serde(default)]
    pub block_comment_end: Option<String>,

    pub chunk_size: Option<usize>,
}

#[derive(Debug, Clone)]
pub enum CommentStyle<'a> {
    /// Single-marker-per-line languages: "#", "//", "--", etc.
    Line(&'a str),
    /// Bracketed block-comment languages: ("<!--", "-->"), ("/*", "*/"), ...
    Block(&'a str, &'a str),
    /// Neither (txt, bare Dockerfile): caller should downgrade to "code" style.
    None,
}

impl Language {
    /// Resolve the preferred comment style. Prefer block-comment when both are
    /// defined (PHP has both `//` and `/* */` — use `/* */`) so the wrapper
    /// always reads as a self-contained comment region.
    pub fn comment_style(&self) -> CommentStyle<'_> {
        if let (Some(s), Some(e)) = (&self.block_comment_start, &self.block_comment_end) {
            if !s.is_empty() && !e.is_empty() {
                return CommentStyle::Block(s.as_str(), e.as_str());
            }
        }
        if let Some(lc) = &self.line_comment {
            if !lc.is_empty() {
                return CommentStyle::Line(lc.as_str());
            }
        }
        CommentStyle::None
    }

    pub fn get_stop_words(&self) -> Vec<String> {
        let mut out = vec![];

        for x in DEFAULT.iter() {
            out.push((*x).to_owned());
        }

        if let Some(line_comment) = &self.line_comment {
            out.push(format!("\n{line_comment}"));
        };

        if let Some(top_level_keywords) = &self.top_level_keywords {
            for word in top_level_keywords {
                out.push(format!("\n{word}"));
            }
        };

        out
    }

    pub fn language(&'static self) -> &'static str {
        self.languages[0].as_str()
    }
}

lazy_static! {
    static ref CONFIG: ConfigList = {
        let mut config_list: ConfigList =
            serdeconv::from_toml_str(include_str!("../assets/languages.toml")).unwrap();
        let mut config = config::Config::load().unwrap();
        config_list.config.append(&mut config.additional_languages);
        config_list
    };
    static ref LANGUAGE_CONFIG_MAPPING: HashMap<&'static str, &'static Language> = {
        let mut map = HashMap::new();
        for c in &CONFIG.config {
            for l in &c.languages {
                assert!(
                    !map.contains_key(l.as_str()),
                    "Duplicate language found: {l}"
                );
                map.insert(l.as_str(), c);
            }
        }
        map
    };
    static ref EXTS_LANGUAGE_MAPPING: HashMap<&'static str, &'static str> = {
        let mut map = HashMap::new();
        for c in &CONFIG.config {
            for e in &c.exts {
                let l = c.language();
                assert!(
                    !map.contains_key(e.as_str()),
                    "Duplicate extension found: {e}"
                );
                map.insert(e.as_str(), l);
            }
        }
        map
    };
    pub static ref UNKNOWN_LANGUAGE: Language = Language {
        languages: vec!["unknown".to_owned()],
        line_comment: Some("".into()),
        block_comment_start: None,
        block_comment_end: None,
        top_level_keywords: Some(vec![]),
        exts: vec![],
        chunk_size: None
    };
}

pub fn get_language(language: &str) -> &'static Language {
    if let Some(lang) = LANGUAGE_CONFIG_MAPPING.get(language) {
        lang
    } else {
        &UNKNOWN_LANGUAGE
    }
}

pub fn get_language_by_ext(ext: &OsStr) -> Option<&'static Language> {
    let ext = ext.to_str()?;
    EXTS_LANGUAGE_MAPPING.get(ext).map(|x| get_language(x))
}

#[cfg(test)]
mod comment_style_tests {
    use super::*;

    #[test]
    fn block_preferred_over_line() {
        let php = get_language("php");
        assert!(matches!(
            php.comment_style(),
            CommentStyle::Block("/*", "*/")
        ));
    }

    #[test]
    fn python_is_block() {
        let python = get_language("python");
        assert!(matches!(
            python.comment_style(),
            CommentStyle::Block("'''", "'''")
        ));
    }

    #[test]
    fn html_is_block() {
        let html = get_language("html");
        assert!(matches!(
            html.comment_style(),
            CommentStyle::Block("<!--", "-->")
        ));
    }

    #[test]
    fn css_is_block() {
        let css = get_language("css");
        assert!(matches!(
            css.comment_style(),
            CommentStyle::Block("/*", "*/")
        ));
    }

    #[test]
    fn ocaml_is_block() {
        let ocaml = get_language("ocaml");
        assert!(matches!(
            ocaml.comment_style(),
            CommentStyle::Block("(*", "*)")
        ));
    }

    #[test]
    fn txt_is_none() {
        let txt = get_language("txt");
        assert!(matches!(txt.comment_style(), CommentStyle::None));
    }

    #[test]
    fn sql_is_block() {
        let sql = get_language("sql");
        assert!(matches!(
            sql.comment_style(),
            CommentStyle::Block("/*", "*/")
        ));
    }

    #[test]
    fn haskell_is_block() {
        let haskell = get_language("haskell");
        assert!(matches!(
            haskell.comment_style(),
            CommentStyle::Block("{-", "-}")
        ));
    }

    #[test]
    fn dockerfile_is_line() {
        let dockerfile = get_language("dockerfile");
        assert!(matches!(
            dockerfile.comment_style(),
            CommentStyle::Line("#")
        ));
    }
}
