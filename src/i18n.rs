//! The words the person reads, in their language.
//!
//! Keyed by the English string rather than by a symbol: the code stays
//! readable without chasing a table to find out what a screen says, and a
//! sentence with no translation falls back to the one written here.
//!
//! The translations are TOML files in `locale/`, one per language, built into
//! the program. A file of the same name in `~/.config/lyricslens/locale/` is
//! read on top of the built-in one, so a language can be added, or a word
//! fixed, without building anything.
//!
//! Code, comments and commits stay in English. This is only what is on screen.

use std::collections::HashMap;
use std::path::Path;
use std::sync::OnceLock;

/// The languages that ship inside the program, by code.
const BUILT_IN: &[(&str, &str)] = &[
    ("es", include_str!("../locale/es.toml")),
    ("pt", include_str!("../locale/pt.toml")),
];

/// The sentence to show, translated when there is a translation.
pub fn t(english: &str) -> String {
    static WORDS: OnceLock<HashMap<String, String>> = OnceLock::new();
    WORDS
        .get_or_init(|| {
            let own = crate::store::config_dir().map(|dir| dir.join("locale"));
            requested()
                .map(|code| words(&code, own.as_deref()))
                .unwrap_or_default()
        })
        .get(english)
        .cloned()
        .unwrap_or_else(|| english.to_owned())
}

/// A sentence with a count in it, `{n}` standing for the number.
///
/// Whole sentences, one for one and one for the rest, rather than a number
/// glued to a phrase: the phrase has to agree with the number in every
/// language there is a file for.
pub fn count(n: usize, one: &str, many: &str) -> String {
    t(if n == 1 { one } else { many }).replace("{n}", &n.to_string())
}

/// The language the session asks for, as a lowercase code such as `pt_br`,
/// or `None` when it asks for English or for nothing in particular.
///
/// The first of the usual variables that is set decides, as it does for
/// every other program.
fn requested() -> Option<String> {
    for name in ["LC_ALL", "LC_MESSAGES", "LANG"] {
        let Ok(value) = std::env::var(name) else {
            continue;
        };
        if value.is_empty() {
            continue;
        }
        let code = value
            .split(['.', '@'])
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase();
        if code.is_empty() || code == "c" || code == "posix" || code.starts_with("en") {
            return None;
        }
        return Some(code);
    }
    None
}

/// Every translated sentence for a language code.
///
/// The bare language comes first and the regional one on top of it, so
/// `pt_br.toml` only needs the words that differ from `pt.toml`. At each
/// step the person's own file, from `own`, wins over the built-in one.
fn words(code: &str, own: Option<&Path>) -> HashMap<String, String> {
    let language = code.split('_').next().unwrap_or(code);
    let mut candidates = vec![language];
    if code != language {
        candidates.push(code);
    }

    let mut words = HashMap::new();
    for candidate in candidates {
        if let Some((_, text)) = BUILT_IN.iter().find(|(name, _)| *name == candidate) {
            merge(&mut words, text, candidate);
        }
        let file = own.map(|dir| dir.join(format!("{candidate}.toml")));
        if let Some(text) = file.and_then(|path| std::fs::read_to_string(path).ok()) {
            merge(&mut words, &text, candidate);
        }
    }
    words
}

/// A file that does not parse is skipped whole: half a language is worse
/// than English.
fn merge(words: &mut HashMap<String, String>, text: &str, code: &str) {
    match toml::from_str::<HashMap<String, String>>(text) {
        Ok(more) => words.extend(more),
        Err(error) => tracing::warn!(%error, code, "a translation file could not be read"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One test, because they all change the same environment variable and
    /// `cargo test` runs them in threads.
    #[test]
    fn what_is_translated_and_what_is_not() {
        let locale = std::env::temp_dir().join("lyricslens-i18n-test");
        let _ = std::fs::remove_dir_all(&locale);
        let own = Some(locale.as_path());

        unsafe { std::env::set_var("LC_ALL", "pt_BR.UTF-8") };
        assert_eq!(requested().as_deref(), Some("pt_br"));
        assert_eq!(
            words("pt_br", own).get("Quit").map(String::as_str),
            Some("Sair")
        );
        assert_eq!(
            words("es_mx", own).get("Quit").map(String::as_str),
            Some("Salir")
        );

        unsafe { std::env::set_var("LC_ALL", "en_GB.UTF-8") };
        assert_eq!(requested(), None);
        unsafe { std::env::set_var("LC_ALL", "C") };
        assert_eq!(requested(), None);

        // A language nobody built in, added by dropping a file in place, and
        // a built-in word corrected the same way.
        std::fs::create_dir_all(&locale).unwrap();
        std::fs::write(locale.join("fr.toml"), "\"Quit\" = \"Quitter\"\n").unwrap();
        std::fs::write(locale.join("es.toml"), "\"Quit\" = \"Cerrar\"\n").unwrap();
        assert_eq!(
            words("fr_fr", own).get("Quit").map(String::as_str),
            Some("Quitter")
        );
        assert_eq!(
            words("es", own).get("Quit").map(String::as_str),
            Some("Cerrar")
        );
        assert_eq!(
            words("es", own).get("Copy").map(String::as_str),
            Some("Copiar")
        );

        // A broken file costs that file, not the language.
        std::fs::write(locale.join("es.toml"), "this is not toml").unwrap();
        assert_eq!(
            words("es", own).get("Quit").map(String::as_str),
            Some("Salir")
        );

        unsafe { std::env::remove_var("LC_ALL") };
        let _ = std::fs::remove_dir_all(&locale);
    }

    /// Sentences no file translates, so the result does not depend on the
    /// language of the machine running the tests.
    #[test]
    fn a_count_picks_the_sentence_for_its_number() {
        assert_eq!(
            count(1, "{n} test apple", "{n} test apples"),
            "1 test apple"
        );
        assert_eq!(
            count(3, "{n} test apple", "{n} test apples"),
            "3 test apples"
        );
        assert_eq!(
            count(0, "{n} test apple", "{n} test apples"),
            "0 test apples"
        );
    }

    /// Every built-in file parses, and none of them is missing a sentence the
    /// others have.
    #[test]
    fn the_built_in_languages_agree() {
        let tables: Vec<(&str, HashMap<String, String>)> = BUILT_IN
            .iter()
            .map(|(code, text)| (*code, toml::from_str(text).expect("a built-in file parses")))
            .collect();
        let (first, reference) = &tables[0];
        for (code, table) in &tables[1..] {
            let mut missing: Vec<_> = reference
                .keys()
                .filter(|key| !table.contains_key(*key))
                .collect();
            missing.extend(table.keys().filter(|key| !reference.contains_key(*key)));
            assert!(
                missing.is_empty(),
                "{first} and {code} differ on {missing:?}"
            );
        }
    }
}
