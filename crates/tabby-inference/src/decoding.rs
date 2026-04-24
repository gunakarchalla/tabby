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
            // For hint + line-comment: stop at first newline (single-line output)
            ("hint", CommentStyle::Line(_)) => vec!["\n".to_string()],
            // For hint + block-comment: stop when closing marker is generated
            ("hint", CommentStyle::Block(_, e)) => vec![e.to_string()],
            // For pseudocode + line-comment: stop at the END marker line
            ("pseudocode", CommentStyle::Line(m)) => vec![format!("\n{m} END")],
            // For pseudocode + block-comment: stop at END or closing marker
            ("pseudocode", CommentStyle::Block(_, e)) => {
                vec!["\nEND".to_string(), e.to_string()]
            }
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
}

impl<'a> StopCondition<'a> {
    pub fn new(stop_trie: Option<CachedTrie<'a>>, text: &str) -> Self {
        Self {
            stop_trie,
            extra_stop_trie: None,
            reversed_text: reverse(text),
            num_decoded: 0,
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
        }
        (false, 0)
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
        let lang = get_language("python");
        let mut cond = factory.create_with_style("", Some(lang), "hint");
        let (should_stop, _) = cond.should_stop("recursively computes fib");
        assert!(!should_stop);
        let (should_stop, _) = cond.should_stop("\n");
        assert!(should_stop);
    }

    #[test]
    fn test_create_with_style_pseudocode_line_stops_on_end_marker() {
        use tabby_common::languages::get_language;
        let factory = StopConditionFactory::default();
        let lang = get_language("python");
        let mut cond = factory.create_with_style("", Some(lang), "pseudocode");
        let (should_stop, _) = cond.should_stop("IF n <= 1 RETURN n");
        assert!(!should_stop);
        // The stop word is "\n# END" — feed it piece by piece
        let (should_stop, _) = cond.should_stop("\n# END");
        assert!(should_stop);
    }
}
