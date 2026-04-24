use std::sync::Arc;

use strfmt::strfmt;
use tabby_common::{
    api::code::{CodeSearch, CodeSearchError, CodeSearchParams, CodeSearchQuery},
    axum::AllowedCodeRepository,
    languages::{get_language, CommentStyle},
};
use tracing::warn;

use super::{Segments, Snippet};

#[derive(Debug, Clone)]
pub struct StyledPrompt {
    /// Full FIM prompt (after template formatting + snippet rewrite).
    pub prompt: String,
    /// Prepend to the raw model output to reconstruct the full visible ghost text.
    pub display_prefix: Option<String>,
    /// Append to the raw model output (deduped) to reconstruct the full visible ghost text.
    pub display_suffix: Option<String>,
    /// The style actually used (may be downgraded to "code" for no-comment languages).
    pub effective_style: String,
}

pub struct PromptBuilder {
    code_search_params: CodeSearchParams,
    prompt_template: Option<String>,
    code: Option<Arc<dyn CodeSearch>>,
}

impl PromptBuilder {
    pub fn new(
        code_search_params: &CodeSearchParams,
        prompt_template: Option<String>,
        code: Option<Arc<dyn CodeSearch>>,
    ) -> Self {
        PromptBuilder {
            code_search_params: code_search_params.clone(),
            prompt_template,
            code,
        }
    }

    fn build_prompt(&self, prefix: String, suffix: String) -> String {
        let Some(prompt_template) = &self.prompt_template else {
            return prefix;
        };

        strfmt!(prompt_template, prefix => prefix, suffix => suffix).unwrap()
    }

    pub async fn collect(
        &self,
        language: &str,
        segments: &Segments,
        allowed_code_repository: &AllowedCodeRepository,
    ) -> Vec<Snippet> {
        let quota_threshold_for_snippets_from_code_search = 256;
        let mut max_snippets_chars_in_prompt = 768;
        let mut snippets: Vec<Snippet> = vec![];

        if let Some((count_characters, snippets_from_segments)) =
            extract_snippets_from_segments(max_snippets_chars_in_prompt, segments)
        {
            max_snippets_chars_in_prompt -= count_characters;
            snippets.extend(snippets_from_segments.into_iter());
        };

        if max_snippets_chars_in_prompt <= quota_threshold_for_snippets_from_code_search {
            return snippets;
        }

        let Some(code) = &self.code else {
            return snippets;
        };

        let Some(git_url) = segments.git_url.as_ref() else {
            return snippets;
        };

        let Some(source_id) = allowed_code_repository.closest_match(git_url) else {
            return snippets;
        };

        let snippets_from_code_search = collect_snippets(
            &self.code_search_params,
            max_snippets_chars_in_prompt,
            code.as_ref(),
            source_id,
            segments.filepath.as_deref(),
            language,
            &segments.prefix,
        )
        .await;

        snippets.extend(snippets_from_code_search.into_iter());
        snippets
    }

    pub fn build(&self, language: &str, segments: Segments, snippets: &[Snippet]) -> String {
        let segments = rewrite_with_snippets(language, segments, snippets);
        self.build_prompt(segments.prefix, get_default_suffix(segments.suffix))
    }

    pub fn build_styled(
        &self,
        language: &str,
        segments: Segments,
        snippets: &[Snippet],
        style: &str,
    ) -> StyledPrompt {
        if style == "code" {
            return StyledPrompt {
                prompt: self.build(language, segments, snippets),
                display_prefix: None,
                display_suffix: None,
                effective_style: "code".into(),
            };
        }

        let lang = get_language(language);
        let comment_style = lang.comment_style();

        let (prefix_injection, suffix_injection, display_prefix, display_suffix) =
            match (style, &comment_style) {
                ("hint", CommentStyle::Line(m)) => (
                    format!("\n{m} hint: "),
                    String::new(),
                    Some(format!("\n{m} hint: ")),
                    None,
                ),
                ("pseudocode", CommentStyle::Line(m)) => (
                    format!("\n{m} BEGIN\n{m} "),
                    format!("\n{m} END\n"),
                    Some(format!("\n{m} BEGIN\n{m} ")),
                    Some(format!("\n{m} END")),
                ),
                ("hint", CommentStyle::Block(s, e)) => (
                    format!("\n{s} hint: "),
                    format!(" {e}\n"),
                    Some(format!("\n{s} hint: ")),
                    Some(format!(" {e}")),
                ),
                ("pseudocode", CommentStyle::Block(s, e)) => (
                    format!("\n{s}\nBEGIN\n  "),
                    format!("\nEND\n{e}\n"),
                    Some(format!("\n{s}\nBEGIN\n  ")),
                    Some(format!("\nEND\n{e}")),
                ),
                // No-comment language: downgrade to code
                _ => {
                    return StyledPrompt {
                        prompt: self.build(language, segments, snippets),
                        display_prefix: None,
                        display_suffix: None,
                        effective_style: "code".into(),
                    };
                }
            };

        let rewritten = rewrite_with_snippets(language, segments, snippets);
        let prefix = rewritten.prefix + prefix_injection.as_str();
        let raw_suffix = rewritten.suffix.unwrap_or_default();
        let suffix = if raw_suffix.is_empty() {
            if suffix_injection.is_empty() {
                "\n".to_string()
            } else {
                suffix_injection
            }
        } else {
            format!("{suffix_injection}{raw_suffix}")
        };

        StyledPrompt {
            prompt: self.build_prompt(prefix, suffix),
            display_prefix,
            display_suffix,
            effective_style: style.to_string(),
        }
    }
}

fn get_default_suffix(suffix: Option<String>) -> String {
    suffix
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "\n".to_string())
}

fn rewrite_with_snippets(language: &str, segments: Segments, snippets: &[Snippet]) -> Segments {
    if snippets.is_empty() {
        segments
    } else {
        let prefix = build_prefix(language, &segments.prefix, snippets);
        Segments { prefix, ..segments }
    }
}

fn build_prefix(language: &str, prefix: &str, snippets: &[Snippet]) -> String {
    if snippets.is_empty() {
        return prefix.to_owned();
    }

    let Some(comment_char) = &get_language(language).line_comment else {
        return prefix.to_owned();
    };

    let mut lines: Vec<String> = vec![];

    for (i, snippet) in snippets.iter().enumerate() {
        lines.push(format!("Path: {}", snippet.filepath));
        for line in snippet.body.lines() {
            lines.push(line.to_owned());
        }

        if i < snippets.len() - 1 {
            lines.push("".to_owned());
        }
    }

    let commented_lines: Vec<String> = lines
        .iter()
        .map(|x| {
            if x.is_empty() {
                comment_char.to_string()
            } else {
                format!("{comment_char} {x}")
            }
        })
        .collect();
    let comments = commented_lines.join("\n");
    format!("{comments}\n{prefix}")
}

fn extract_snippets_from_segments(
    max_snippets_chars: usize,
    segments: &Segments,
) -> Option<(usize, Vec<Snippet>)> {
    let mut count_characters = 0;
    let mut ret = Vec::new();

    // declarations has highest priority.
    if let Some(declarations) = &segments.declarations {
        for declaration in declarations {
            if count_characters + declaration.body.len() > max_snippets_chars {
                break;
            }
            count_characters += declaration.body.len();
            ret.push(Snippet {
                filepath: declaration.filepath.clone(),
                body: declaration.body.clone(),
                score: 1.0,
            });
        }
    }

    // then comes to the snippets from changed files.
    if let Some(relevant_snippets) = &segments.relevant_snippets_from_changed_files {
        for snippet in relevant_snippets {
            if count_characters + snippet.body.len() > max_snippets_chars {
                break;
            }

            count_characters += snippet.body.len();
            ret.push(Snippet {
                filepath: snippet.filepath.clone(),
                body: snippet.body.clone(),
                score: 1.0,
            });
        }
    }

    // then comes to the snippets from recently opened files.
    if let Some(relevant_snippets) = &segments.relevant_snippets_from_recently_opened_files {
        for snippet in relevant_snippets {
            if count_characters + snippet.body.len() > max_snippets_chars {
                break;
            }

            count_characters += snippet.body.len();
            ret.push(Snippet {
                filepath: snippet.filepath.clone(),
                body: snippet.body.clone(),
                score: 1.0,
            });
        }
    }

    if ret.is_empty() {
        None
    } else {
        Some((count_characters, ret))
    }
}

async fn collect_snippets(
    code_search_params: &CodeSearchParams,
    max_snippets_chars: usize,
    code: &dyn CodeSearch,
    source_id: &str,
    filepath: Option<&str>,
    language: &str,
    content: &str,
) -> Vec<Snippet> {
    let query = CodeSearchQuery::new(
        filepath.map(|x| x.to_owned()),
        Some(language.to_owned()),
        content.to_owned(),
        source_id.to_owned(),
    );

    let mut ret = Vec::new();

    let serp = match code
        .search_in_language(query, code_search_params.clone())
        .await
    {
        Ok(serp) => serp,
        Err(CodeSearchError::NotReady) => {
            // Ignore.
            return vec![];
        }
        Err(CodeSearchError::TantivyError(err)) => {
            warn!("Failed to search: {}", err);
            return ret;
        }
        Err(CodeSearchError::QueryParserError(err)) => {
            warn!("Failed to parse query: {}", err);
            return ret;
        }
        Err(CodeSearchError::Other(err)) => {
            warn!("Failed to search: {}", err);
            return ret;
        }
    };

    let mut count_characters = 0;
    for hit in serp.hits {
        let body = hit.doc.body;

        if count_characters + body.len() > max_snippets_chars {
            break;
        }

        count_characters += body.len();
        ret.push(Snippet {
            filepath: hit.doc.filepath,
            body,
            score: hit.scores.rrf,
        });
    }

    ret
}

#[cfg(test)]
mod tests {
    use async_trait::async_trait;
    use tabby_common::api::code::CodeSearchResponse;

    use super::*;
    use crate::services::completion::Declaration;

    fn create_prompt_builder(with_template: bool) -> PromptBuilder {
        let prompt_template = if with_template {
            // Init prompt builder with codellama prompt template
            Some("<PRE> {prefix} <SUF>{suffix} <MID>".into())
        } else {
            None
        };

        // Init prompt builder with prompt rewrite disabled.
        PromptBuilder::new(&CodeSearchParams::default(), prompt_template, None)
    }

    fn make_segment(prefix: String, suffix: Option<String>) -> Segments {
        Segments {
            prefix,
            suffix,
            filepath: None,
            git_url: None,
            declarations: None,
            relevant_snippets_from_changed_files: None,
            relevant_snippets_from_recently_opened_files: None,
            clipboard: None,
            edit_history: None,
        }
    }

    struct MockCodeSearch(fn() -> Result<CodeSearchResponse, CodeSearchError>);

    #[async_trait]
    impl CodeSearch for MockCodeSearch {
        async fn search_in_language(
            &self,
            _query: CodeSearchQuery,
            _params: CodeSearchParams,
        ) -> Result<CodeSearchResponse, CodeSearchError> {
            (self.0)()
        }
    }

    #[tokio::test]
    async fn test_collect_snippets() {
        // Not ready error from CodeSearch should result in empty snippets, rather than error
        let search = MockCodeSearch(|| Err(CodeSearchError::NotReady));
        let snippets =
            collect_snippets(&CodeSearchParams::default(), 150, &search, "", None, "", "").await;
        assert_eq!(snippets, vec![]);

        let search = MockCodeSearch(|| {
            Ok(CodeSearchResponse {
                hits: vec![Default::default()],
            })
        });
        let snippets =
            collect_snippets(&CodeSearchParams::default(), 150, &search, "", None, "", "").await;
        assert_eq!(
            snippets,
            vec![Snippet {
                filepath: "".into(),
                body: "".into(),
                score: 0.0
            }]
        );
    }

    #[test]
    fn test_prompt_template() {
        let pb = create_prompt_builder(true);

        // Rewrite disabled, so the language doesn't matter.
        let language = "python";
        let snippets = &vec![];

        // Test w/ prefix, w/ suffix.
        {
            let segments = make_segment(
                "this is some prefix".into(),
                Some("this is some suffix".into()),
            );
            assert_eq!(
                pb.build(language, segments, snippets),
                "<PRE> this is some prefix <SUF>this is some suffix <MID>"
            );
        }

        // Test w/ prefix, w/o suffix.
        {
            let segments = make_segment("this is some prefix".into(), None);
            assert_eq!(
                pb.build(language, segments, snippets),
                "<PRE> this is some prefix <SUF>\n <MID>"
            );
        }

        // Test w/ prefix, w/ empty suffix.
        {
            let segments = make_segment("this is some prefix".into(), Some("".into()));
            assert_eq!(
                pb.build(language, segments, snippets),
                "<PRE> this is some prefix <SUF>\n <MID>"
            );
        }

        // Test w/ empty prefix, w/ suffix.
        {
            let segments = make_segment("".into(), Some("this is some suffix".into()));
            assert_eq!(
                pb.build(language, segments, snippets),
                "<PRE>  <SUF>this is some suffix <MID>"
            );
        }

        // Test w/ empty prefix, w/o suffix.
        {
            let segments = make_segment("".into(), None);
            assert_eq!(
                pb.build(language, segments, snippets),
                "<PRE>  <SUF>\n <MID>"
            );
        }

        // Test w/ emtpy prefix, w/ empty suffix.
        {
            let segments = make_segment("".into(), Some("".into()));
            assert_eq!(
                pb.build(language, segments, snippets),
                "<PRE>  <SUF>\n <MID>"
            );
        }
    }

    #[test]
    fn test_no_prompt_template() {
        let pb = create_prompt_builder(false);

        // Rewrite disabled, so the language doesn't matter.
        let language = "python";
        let snippets = &vec![];

        // Test w/ prefix, w/ suffix.
        {
            let segments = make_segment(
                "this is some prefix".into(),
                Some("this is some suffix".into()),
            );
            assert_eq!(
                pb.build(language, segments, snippets),
                "this is some prefix"
            );
        }

        // Test w/ prefix, w/o suffix.
        {
            let segments = make_segment("this is some prefix".into(), None);
            assert_eq!(
                pb.build(language, segments, snippets),
                "this is some prefix"
            );
        }

        // Test w/ prefix, w/ empty suffix.
        {
            let segments = make_segment("this is some prefix".into(), Some("".into()));
            assert_eq!(
                pb.build(language, segments, snippets),
                "this is some prefix"
            );
        }

        // Test w/ empty prefix, w/ suffix.
        {
            let segments = make_segment("".into(), Some("this is some suffix".into()));
            assert_eq!(pb.build(language, segments, snippets), "");
        }

        // Test w/ empty prefix, w/o suffix.
        {
            let segments = make_segment("".into(), None);
            assert_eq!(pb.build(language, segments, snippets), "");
        }

        // Test w/ empty prefix, w/ empty suffix.
        {
            let segments = make_segment("".into(), Some("".into()));
            assert_eq!(pb.build(language, segments, snippets), "");
        }
    }

    #[test]
    fn test_build_styled_python_hint() {
        let pb = create_prompt_builder(true);
        let seg = make_segment("def foo():\n    ".into(), Some("\n".into()));
        let styled = pb.build_styled("python", seg, &[], "hint");
        assert_eq!(styled.effective_style, "hint");
        assert!(styled.prompt.contains("# hint: "));
        assert_eq!(styled.display_prefix.as_deref(), Some("\n# hint: "));
        assert!(styled.display_suffix.is_none());
    }

    #[test]
    fn test_build_styled_python_pseudocode() {
        let pb = create_prompt_builder(true);
        let seg = make_segment("def fib(n):\n    ".into(), Some("\n".into()));
        let styled = pb.build_styled("python", seg, &[], "pseudocode");
        assert_eq!(styled.effective_style, "pseudocode");
        assert!(styled.prompt.contains("# BEGIN"));
        assert!(styled.prompt.contains("# END"));
        assert_eq!(styled.display_prefix.as_deref(), Some("\n# BEGIN\n# "));
        assert_eq!(styled.display_suffix.as_deref(), Some("\n# END"));
    }

    #[test]
    fn test_build_styled_html_pseudocode() {
        let pb = create_prompt_builder(true);
        let seg = make_segment("<body>\n".into(), Some("</body>".into()));
        let styled = pb.build_styled("html", seg, &[], "pseudocode");
        assert_eq!(styled.effective_style, "pseudocode");
        assert!(styled.prompt.contains("<!--"));
        assert!(styled.prompt.contains("BEGIN"));
        assert!(styled.prompt.contains("END"));
        assert!(styled.prompt.contains("-->"));
    }

    #[test]
    fn test_build_styled_html_hint() {
        let pb = create_prompt_builder(true);
        let seg = make_segment("<body>\n".into(), Some("</body>".into()));
        let styled = pb.build_styled("html", seg, &[], "hint");
        assert_eq!(styled.effective_style, "hint");
        assert!(styled.prompt.contains("<!-- hint:"));
        assert_eq!(styled.display_prefix.as_deref(), Some("\n<!-- hint: "));
        assert_eq!(styled.display_suffix.as_deref(), Some(" -->"));
    }

    #[test]
    fn test_build_styled_code_passthrough() {
        let pb = create_prompt_builder(true);
        let seg = make_segment("def foo():\n    ".into(), Some("\n".into()));
        let styled = pb.build_styled("python", seg.clone(), &[], "code");
        let plain = pb.build("python", seg, &[]);
        assert_eq!(styled.effective_style, "code");
        assert_eq!(styled.prompt, plain);
        assert!(styled.display_prefix.is_none());
        assert!(styled.display_suffix.is_none());
    }

    #[test]
    fn test_build_prefix_readable() {
        let snippets = vec![
            Snippet {
                filepath: "a1.py".to_owned(),
                body: "res_1 = invoke_function_1(n)".to_owned(),
                score: 1.0,
            },
            Snippet {
                filepath: "a2.py".to_owned(),
                body: "res_2 = invoke_function_2(n)".to_owned(),
                score: 1.0,
            },
            Snippet {
                filepath: "a3.py".to_owned(),
                body: "res_3 = invoke_function_3(n)".to_owned(),
                score: 1.0,
            },
        ];

        let prefix = "\
'''
Use some invoke_function to do some job.
'''
def this_is_prefix():\n";

        let expected_built_prefix = "\
# Path: a1.py
# res_1 = invoke_function_1(n)
#
# Path: a2.py
# res_2 = invoke_function_2(n)
#
# Path: a3.py
# res_3 = invoke_function_3(n)
'''
Use some invoke_function to do some job.
'''
def this_is_prefix():\n";

        assert_eq!(
            build_prefix("python", prefix, &snippets),
            expected_built_prefix
        );
    }

    #[test]
    fn it_extract_snippets_from_segments() {
        let segments = Segments {
            prefix: "def fib(n):\n    ".to_string(),
            suffix: Some("\n        return fib(n - 1) + fib(n - 2)".to_string()),
            filepath: None,
            git_url: None,
            declarations: None,
            relevant_snippets_from_changed_files: None,
            relevant_snippets_from_recently_opened_files: None,
            clipboard: None,
            edit_history: None,
        };

        let max_snippets_chars = 768;
        assert!(extract_snippets_from_segments(max_snippets_chars, &segments).is_none());

        let segments = Segments {
            prefix: "def fib(n):\n    ".to_string(),
            suffix: Some("\n        return fib(n - 1) + fib(n - 2)".to_string()),
            filepath: None,
            git_url: None,
            declarations: Some(vec![Declaration {
                filepath: "file:///path/to/file.py".to_string(),
                body: "def fib(n):\n    return n if n <= 1 else fib(n - 1) + fib(n - 2)"
                    .to_string(),
            }]),
            relevant_snippets_from_changed_files: Some(vec![Snippet {
                filepath: "a1.py".to_owned(),
                body: "res_1 = invoke_function_1(n)".to_owned(),
                score: 1.0,
            }]),
            relevant_snippets_from_recently_opened_files: Some(vec![Snippet {
                filepath: "b1.py".to_owned(),
                body: "res_1 = invoke_function_1(n)".to_owned(),
                score: 1.0,
            }]),
            clipboard: None,
            edit_history: None,
        };

        assert!(
            extract_snippets_from_segments(max_snippets_chars, &segments)
                .is_some_and(|x| x.1.len() == 3)
        );
    }
}
