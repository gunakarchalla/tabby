use dashmap::DashMap;
use tabby_common::languages::Language;
use trie_rs::{Trie, TrieBuilder};

pub struct StopConditionFactory {
    stop_trie_cache: DashMap<String, Trie<u8>>,
    stop_words_from_model_config: Vec<String>,
}

fn reverse<T>(s: T) -> String
where
    T: Into<String>,
{
    s.into().chars().rev().collect()
}

impl Default for StopConditionFactory {
    fn default() -> Self {
        Self {
            stop_trie_cache: DashMap::new(),
            stop_words_from_model_config: vec![],
        }
    }
}

type CachedTrie<'a> = dashmap::mapref::one::Ref<'a, String, Trie<u8>>;

impl StopConditionFactory {
    pub fn with_stop_words(stop_words: Vec<String>) -> Self {
        Self {
            stop_trie_cache: DashMap::new(),
            stop_words_from_model_config: stop_words,
        }
    }

    pub fn create(&self, text: &str, language: Option<&'static Language>) -> StopCondition<'_> {
        if let Some(language) = language {
            StopCondition::new(self.get_trie(language), text)
        } else {
            StopCondition::new(None, text)
        }
    }

    pub fn create_with_style(
        &self,
        text: &str,
        language: Option<&'static Language>,
        style: &str,
    ) -> StopCondition<'_> {
        if style == "code" {
            return self.create(text, language);
        }
        let Some(language) = language else {
            return self.create(text, None);
        };

        use tabby_common::languages::CommentStyle;
        let extra: Vec<String> = match (style, language.comment_style()) {
            ("stochastic", CommentStyle::Line(m)) => {
                let cached = self.get_trie(language);
                return StopCondition::new_with_stochastic_line(cached, m.to_string(), text);
            }
            ("stochastic", CommentStyle::Block(_, e)) => vec![e.to_string()],
            // hint + line: no closing marker in the suffix; stop at first newline.
            ("hint", CommentStyle::Line(_)) => vec!["\n".to_string()],
            // hint + block: stop when the model writes the closing block marker.
            ("hint", CommentStyle::Block(_, e)) => vec![e.to_string()],
            // pseudocode + line: trust the model to follow the suffix
            // "\n{m} END\n"; stop once it has produced the END line (before the
            // trailing newline so dedup is clean).
            ("pseudocode", CommentStyle::Line(m)) => vec![format!("\n{m} END")],
            // pseudocode + block: trust the model to follow the suffix
            // "\nEND {e}\n"; stop on the closing block marker, which only
            // appears once the END frame is complete.
            ("pseudocode", CommentStyle::Block(_, e)) => vec![e.to_string()],
            _ => vec![],
        };

        let cached_trie = self.get_trie(language);

        if extra.is_empty() {
            return StopCondition::new(cached_trie, text);
        }

        let extra_trie = create_stop_trie(extra);
        StopCondition::new_with_extra_trie(cached_trie, Some(extra_trie), text)
    }

    fn get_trie<'a>(&'a self, language: &'static Language) -> Option<CachedTrie<'a>> {
        let mut stop_words = language.get_stop_words();
        // append model stop words
        stop_words.extend(self.stop_words_from_model_config.iter().cloned());

        if stop_words.is_empty() {
            None
        } else {
            let hashkey = language.language().to_owned();
            let mut trie = self.stop_trie_cache.get(&hashkey);
            if trie.is_none() {
                self.stop_trie_cache
                    .insert(hashkey.clone(), create_stop_trie(stop_words));
                trie = self.stop_trie_cache.get(&hashkey);
            }

            trie
        }
    }
}

fn create_stop_trie(stop_words: Vec<String>) -> Trie<u8> {
    let mut builder = TrieBuilder::new();
    for word in stop_words {
        builder.push(reverse(word))
    }
    builder.build()
}

pub struct StopCondition<'a> {
    stop_trie: Option<CachedTrie<'a>>,
    extra_stop_trie: Option<Trie<u8>>,
    reversed_text: String,
    num_decoded: usize,
    forward_text: String,
    stochastic_line_marker: Option<String>,
}

impl<'a> StopCondition<'a> {
    pub fn new(stop_trie: Option<CachedTrie<'a>>, text: &str) -> Self {
        Self {
            stop_trie,
            extra_stop_trie: None,
            reversed_text: reverse(text),
            num_decoded: 0,
            forward_text: String::new(),
            stochastic_line_marker: None,
        }
    }

    fn new_with_extra_trie(
        stop_trie: Option<CachedTrie<'a>>,
        extra_stop_trie: Option<Trie<u8>>,
        text: &str,
    ) -> Self {
        Self {
            stop_trie,
            extra_stop_trie,
            reversed_text: reverse(text),
            num_decoded: 0,
            forward_text: String::new(),
            stochastic_line_marker: None,
        }
    }

    pub fn new_with_stochastic_line(
        stop_trie: Option<CachedTrie<'a>>,
        marker: String,
        text: &str,
    ) -> Self {
        Self {
            stop_trie,
            extra_stop_trie: None,
            reversed_text: reverse(text),
            num_decoded: 0,
            forward_text: text.to_string(),
            stochastic_line_marker: Some(marker),
        }
    }

    pub fn should_stop(&mut self, new_text: &str) -> (bool, usize) {
        self.num_decoded += 1;
        if !new_text.is_empty() {
            self.reversed_text = reverse(new_text) + &self.reversed_text;

            if let Some(re) = &self.stop_trie {
                let matches = re.common_prefix_search(&self.reversed_text);
                let matched_length = matches.into_iter().map(|x| x.len()).max();
                if let Some(matched_length) = matched_length {
                    return (true, matched_length);
                }
            }

            if let Some(extra) = &self.extra_stop_trie {
                let matches = extra.common_prefix_search(&self.reversed_text);
                let matched_length = matches.into_iter().map(|x| x.len()).max();
                if let Some(matched_length) = matched_length {
                    return (true, matched_length);
                }
            }

            if self.stochastic_line_marker.is_some() {
                self.forward_text.push_str(new_text);
                let marker = self.stochastic_line_marker.as_deref().unwrap();
                if let Some(matched) = check_stochastic_line_end(&self.forward_text, marker) {
                    return (true, matched);
                }
            }
        }
        (false, 0)
    }
}

/// For stochastic + line-comment: stop the moment a `\n` is followed by any
/// content that is not a prefix of the comment marker.
///
/// Returns the number of bytes (from `\n` inclusive) to truncate from the end
/// of the generated text, or `None` if we should keep going.
fn check_stochastic_line_end(text: &str, marker: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    let nl_pos = bytes.iter().rposition(|&b| b == b'\n')?;
    let after = &bytes[nl_pos + 1..];
    if after.is_empty() {
        return None; // newline just emitted; wait for the next char(s)
    }
    let m = marker.as_bytes();
    let n = after.len().min(m.len());
    if after[..n] != m[..n] {
        Some(text.len() - nl_pos)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {

    use tabby_common::languages::UNKNOWN_LANGUAGE;

    use super::*;

    #[test]
    fn test_trie_works() {
        let text = reverse("void write_u32(std::uint32_t val) const {\n        write_raw(&val, sizeof(val));\n    }\n\n    ~llama_file() {\n        if (fp) {\n            std::fclose(fp);\n        }\n    }\n};\n\nvoid");

        let trie = create_stop_trie(vec!["\n\n".to_owned(), "\n\n  ".to_owned()]);
        assert!(trie.common_prefix_search(&text).is_empty());

        let trie = create_stop_trie(vec![
            "\n\n".to_owned(),
            "\n\n  ".to_owned(),
            "\nvoid".to_owned(),
            "<|file_sep|>".to_owned(), // qwen 2.5 coder style
        ]);
        assert!(!trie.common_prefix_search(&text).is_empty());

        let qwen25coder = reverse("qwen25 style stop words;<|file_sep|>");
        assert!(!trie.common_prefix_search(qwen25coder).is_empty());
    }

    #[test]
    fn test_stop_condition_max_length() {
        let factory = StopConditionFactory::default();
        let mut cond = factory.create("", Some(&UNKNOWN_LANGUAGE));
        let (should_stop, _) = cond.should_stop("1");
        assert!(!should_stop);
        let (should_stop, _) = cond.should_stop("2");
        assert!(!should_stop);
        let (should_stop, _) = cond.should_stop("3");
        assert!(!should_stop);
        let (should_stop, _) = cond.should_stop("4");
        assert!(!should_stop)
    }

    #[test]
    fn test_stop_condition_additional_stop_words() {
        let factory = StopConditionFactory::with_stop_words(vec!["<|endoftext|>".to_owned()]);
        let mut cond = factory.create("", Some(&UNKNOWN_LANGUAGE));
        let (should_stop, _) = cond.should_stop("1");
        assert!(!should_stop);
        let (should_stop, _) = cond.should_stop("<|endoftext|>");
        assert!(should_stop);
    }

    #[test]
    fn test_create_with_style_code_delegates_to_create() {
        use tabby_common::languages::get_language;
        let factory = StopConditionFactory::default();
        let lang = get_language("python");
        let mut cond = factory.create_with_style("", Some(lang), "code");
        // Should behave like a normal stop condition (no extra trie)
        let (should_stop, _) = cond.should_stop("x = 1");
        assert!(!should_stop);
    }

    #[test]
    fn test_create_with_style_hint_line_stops_on_newline() {
        use tabby_common::languages::get_language;
        let factory = StopConditionFactory::default();
        // Dockerfile is pure-line ("#"), so its comment_style stays Line.
        let lang = get_language("dockerfile");
        let mut cond = factory.create_with_style("", Some(lang), "hint");
        let (should_stop, _) = cond.should_stop("base alpine image");
        assert!(!should_stop);
        let (should_stop, _) = cond.should_stop("\n");
        assert!(should_stop);
    }

    #[test]
    fn test_create_with_style_pseudocode_line_stops_on_end_marker() {
        use tabby_common::languages::get_language;
        let factory = StopConditionFactory::default();
        let lang = get_language("dockerfile");
        let mut cond = factory.create_with_style("", Some(lang), "pseudocode");
        let (should_stop, _) = cond.should_stop("INSTALL deps");
        assert!(!should_stop);
        // Stop word is "\n# END".
        let (should_stop, _) = cond.should_stop("\n# END");
        assert!(should_stop);
    }

    #[test]
    fn test_create_with_style_hint_block_stops_on_close() {
        use tabby_common::languages::get_language;
        let factory = StopConditionFactory::default();
        // Python now resolves to Block ("'''", "'''").
        let lang = get_language("python");
        let mut cond = factory.create_with_style("", Some(lang), "hint");
        let (should_stop, _) = cond.should_stop("computes fib recursively");
        assert!(!should_stop);
        let (should_stop, _) = cond.should_stop("'''");
        assert!(should_stop);
    }

    #[test]
    fn test_create_with_style_pseudocode_block_stops_on_close() {
        use tabby_common::languages::get_language;
        let factory = StopConditionFactory::default();
        let lang = get_language("python");
        let mut cond = factory.create_with_style("", Some(lang), "pseudocode");
        // Body of the pseudocode should not trigger a stop.
        let (should_stop, _) = cond.should_stop("IF n <= 1 RETURN n");
        assert!(!should_stop);
        // Nor should the bare "\nEND " segment — we trust the model to follow
        // the suffix injection through to the closing marker.
        let (should_stop, _) = cond.should_stop("\nEND ");
        assert!(!should_stop);
        // Closing block marker fires the stop.
        let (should_stop, _) = cond.should_stop("'''");
        assert!(should_stop);
    }

    #[test]
    fn test_create_with_style_stochastic_line_stops_on_non_comment() {
        use tabby_common::languages::get_language;
        let factory = StopConditionFactory::default();
        let lang = get_language("dockerfile"); // marker "#"
        let mut cond = factory.create_with_style("", Some(lang), "stochastic");
        let (should_stop, _) = cond.should_stop(" Step one");
        assert!(!should_stop);
        let (should_stop, _) = cond.should_stop("\n# 2: Step two");
        assert!(!should_stop);
        let (should_stop, n) = cond.should_stop("\nFROM alpine");
        assert!(should_stop);
        assert!(n >= "\nFROM alpine".len());
    }

    #[test]
    fn test_create_with_style_stochastic_line_partial_marker_waits() {
        // Test the partial-prefix logic of check_stochastic_line_end directly
        // using new_with_stochastic_line, since no language in the config
        // produces CommentStyle::Line("--") (all "--" languages also have block comments).
        let mut cond = StopCondition::new_with_stochastic_line(None, "--".to_string(), "");
        // After "\n-" we have a partial-prefix match of "--"; do not stop.
        let (should_stop, _) = cond.should_stop("\n-");
        assert!(!should_stop);
        // Completing the marker keeps us going.
        let (should_stop, _) = cond.should_stop("- 2: next step");
        assert!(!should_stop);
        // Now a divergence after a newline.
        let (should_stop, _) = cond.should_stop("\nSELECT");
        assert!(should_stop);
    }

    #[test]
    fn test_create_with_style_stochastic_block_stops_on_close() {
        use tabby_common::languages::get_language;
        let factory = StopConditionFactory::default();
        let lang = get_language("python"); // Block ("'''", "'''")
        let mut cond = factory.create_with_style("", Some(lang), "stochastic");
        let (should_stop, _) = cond.should_stop(" Parse input\n2: Validate\n3: Compute");
        assert!(!should_stop);
        let (should_stop, _) = cond.should_stop("'''");
        assert!(should_stop);
    }
}
