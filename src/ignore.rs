//! What a tree is packed without: the format's default exclusions and the
//! `.mcpbignore` at its root.
//!
//! Patterns are the `.gitignore` subset the format documents: one per line, `#`
//! comments, `!` to re-include, a trailing `/` for directories, `*` and `?`
//! within a name, `**` across names. A pattern with a leading or inner `/` is
//! anchored to the root; any other matches at any depth.

use std::fs;
use std::io::ErrorKind;
use std::path::Path;

use crate::Error;

/// The format's default exclusions, from its CLI's documentation.
const EXCLUDED_BY_DEFAULT: [&str; 21] = [
    ".DS_Store",
    "Thumbs.db",
    ".gitignore",
    ".git/",
    "*.log",
    "npm-debug.log*",
    "yarn-debug.log*",
    "yarn-error.log*",
    ".npm/",
    ".npmrc",
    ".yarnrc",
    ".yarn/",
    ".pnp.*",
    "node_modules/.cache/",
    "node_modules/.bin/",
    "*.map",
    ".env.local",
    ".env.*.local",
    "package-lock.json",
    "yarn.lock",
    // Not in the format's list, but the ignore file is not part of the server.
    ".mcpbignore",
];

/// The exclusions for one tree.
#[derive(Debug, Default)]
pub(crate) struct Exclusions {
    rules: Vec<Rule>,
}

#[derive(Debug)]
struct Rule {
    pattern: String,
    /// A `!` pattern, which re-includes.
    negated: bool,
    /// Matches only directories, and everything in them.
    directory: bool,
    /// Matches from the root only: the pattern has a leading or inner `/`.
    anchored: bool,
}

impl Exclusions {
    /// The default exclusions plus `dir/.mcpbignore`, if present.
    pub(crate) fn at(dir: &Path) -> Result<Self, Error> {
        let mut rules = EXCLUDED_BY_DEFAULT
            .iter()
            .map(|pattern| Rule::new(pattern))
            .collect::<Vec<_>>();

        let ignore = dir.join(".mcpbignore");
        let text = match fs::read_to_string(&ignore) {
            Ok(text) => text,
            Err(error) if error.kind() == ErrorKind::NotFound => String::new(),
            Err(error) => return Err(Error::read(&ignore, error)),
        };
        rules.extend(
            text.lines()
                .map(str::trim)
                .filter(|line| !line.is_empty() && !line.starts_with('#'))
                .map(Rule::new),
        );

        Ok(Self { rules })
    }

    /// Whether `name`, relative to the tree's root, is left out. An excluded
    /// directory excludes its contents unless a later `!` pattern re-includes them.
    pub(crate) fn excludes(&self, name: &str, directory: bool) -> bool {
        let parts = name.split('/').collect::<Vec<_>>();
        let mut excluded = false;

        for rule in &self.rules {
            let mut matches = rule.matches(name, directory);
            for depth in 1..parts.len() {
                if matches {
                    break;
                }
                matches = rule.matches(&parts[..depth].join("/"), true);
            }
            if matches {
                excluded = !rule.negated;
            }
        }

        excluded
    }

    /// Whether a `!` pattern could re-include a file, the only reason to walk an
    /// excluded directory.
    pub(crate) fn keeps_files(&self) -> bool {
        self.rules.iter().any(|rule| rule.negated)
    }
}

impl Rule {
    fn new(line: &str) -> Self {
        let mut line = line;
        let negated = line.starts_with('!');
        if negated {
            line = &line[1..];
        }
        let directory = line.ends_with('/');
        let rooted = line.starts_with('/') || line.starts_with("./");
        let pattern = line
            .trim_start_matches("./")
            .trim_start_matches('/')
            .trim_end_matches('/');

        Self {
            pattern: pattern.to_string(),
            negated,
            directory,
            anchored: rooted || pattern.contains('/'),
        }
    }

    fn matches(&self, name: &str, directory: bool) -> bool {
        if self.directory && !directory {
            return false;
        }
        matches_pattern(&self.pattern, self.anchored, name)
    }
}

/// Whether `name` matches `pattern`: from the root if `anchored`, else at any depth.
fn matches_pattern(pattern: &str, anchored: bool, name: &str) -> bool {
    let parts = name.split('/').collect::<Vec<_>>();

    if !anchored {
        // Any one segment, as in `.gitignore`.
        return parts.iter().any(|part| matches_segment(pattern, part));
    }

    matches_parts(&pattern.split('/').collect::<Vec<_>>(), &parts)
}

fn matches_parts(pattern: &[&str], parts: &[&str]) -> bool {
    let Some((first, rest)) = pattern.split_first() else {
        return parts.is_empty();
    };
    if *first == "**" {
        return (0..=parts.len()).any(|skip| matches_parts(rest, &parts[skip..]));
    }

    match parts.split_first() {
        Some((part, parts)) if matches_segment(first, part) => matches_parts(rest, parts),
        _ => false,
    }
}

/// Whether one path segment matches a glob of `*` and `?`.
fn matches_segment(pattern: &str, name: &str) -> bool {
    fn matches(pattern: &[char], name: &[char]) -> bool {
        match pattern.split_first() {
            None => name.is_empty(),
            Some(('*', rest)) => {
                matches(rest, name) || (!name.is_empty() && matches(pattern, &name[1..]))
            }
            Some(('?', rest)) => !name.is_empty() && matches(rest, &name[1..]),
            Some((first, rest)) => match name.split_first() {
                Some((next, name)) if next == first => matches(rest, name),
                _ => false,
            },
        }
    }

    matches(
        &pattern.chars().collect::<Vec<_>>(),
        &name.chars().collect::<Vec<_>>(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules(lines: &[&str]) -> Exclusions {
        Exclusions {
            rules: EXCLUDED_BY_DEFAULT
                .iter()
                .chain(lines)
                .map(|line| Rule::new(line))
                .collect(),
        }
    }

    #[test]
    fn a_pattern_without_a_slash_matches_at_any_depth() {
        let exclusions = rules(&["*.test.js"]);
        assert!(!exclusions.excludes("a.js", false));
        assert!(exclusions.excludes("a.test.js", false));
        assert!(exclusions.excludes("test/a.test.js", false));
        assert!(exclusions.excludes("test/deep/a.test.js", false));
    }

    #[test]
    fn a_slash_anchors_a_pattern_at_the_root() {
        let exclusions = rules(&["src/*.test.ts"]);
        assert!(exclusions.excludes("src/a.test.ts", false));
        assert!(!exclusions.excludes("nested/src/a.test.ts", false));
    }

    #[test]
    fn a_leading_slash_anchors_a_name_at_the_root() {
        for pattern in ["/build", "./build", "/build/"] {
            let exclusions = rules(&[pattern]);
            assert!(exclusions.excludes("build", true), "{pattern}");
            assert!(exclusions.excludes("build/out.txt", false), "{pattern}");
            assert!(!exclusions.excludes("lib/build", true), "{pattern}");
            assert!(
                !exclusions.excludes("lib/build/out.txt", false),
                "{pattern}"
            );
        }
    }

    #[test]
    fn a_directory_without_a_slash_is_left_out_anywhere() {
        // As in `.gitignore`, a trailing `/` marks a directory, it does not
        // anchor the pattern.
        let exclusions = rules(&["coverage/"]);
        assert!(exclusions.excludes("coverage/lcov.info", false));
        assert!(exclusions.excludes("nested/coverage/lcov.info", false));
    }

    #[test]
    fn two_stars_cross_names() {
        let exclusions = rules(&["src/**/*.test.ts"]);
        assert!(exclusions.excludes("src/a.test.ts", false));
        assert!(exclusions.excludes("src/deep/b/a.test.ts", false));
        assert!(!exclusions.excludes("src/a.ts", false));
    }

    #[test]
    fn a_directory_takes_what_is_in_it() {
        let exclusions = rules(&["temp/"]);
        assert!(exclusions.excludes("temp", true));
        assert!(exclusions.excludes("temp/a.txt", false));
        assert!(exclusions.excludes("temp/deep/a.txt", false));
    }

    #[test]
    fn a_directory_pattern_does_not_match_a_file_of_that_name() {
        let exclusions = rules(&["docs/"]);
        assert!(!exclusions.excludes("docs", false));
        assert!(exclusions.excludes("docs", true));
    }

    #[test]
    fn a_later_pattern_takes_a_file_back_in() {
        let exclusions = rules(&["temp/", "!temp/keep.txt"]);
        assert!(exclusions.excludes("temp/drop.txt", false));
        assert!(!exclusions.excludes("temp/keep.txt", false));
        assert!(exclusions.keeps_files());
    }

    #[test]
    fn the_formats_own_exclusions_apply_without_a_mcpbignore() {
        let exclusions = rules(&[]);
        for left_out in [
            ".DS_Store",
            ".git/objects/x",
            "node_modules/.cache/x",
            "app.log",
            "index.js.map",
            "yarn.lock",
            "package-lock.json",
            ".mcpbignore",
        ] {
            assert!(exclusions.excludes(left_out, false), "{left_out}");
        }
        for kept in [
            "server/index.js",
            "node_modules/left-pad/index.js",
            "README.md",
        ] {
            assert!(!exclusions.excludes(kept, false), "{kept}");
        }
    }

    #[test]
    fn a_question_mark_is_one_character() {
        assert!(matches_segment("a?c", "abc"));
        assert!(!matches_segment("a?c", "ac"));
        assert!(!matches_segment("a?c", "abbc"));
        assert!(matches_segment("*", "anything"));
        assert!(matches_segment("*", ""));
        assert!(matches_segment("a*", "a"));
        assert!(!matches_segment("a*", "b"));
    }

    #[test]
    fn what_is_not_a_glob_is_a_name() {
        assert!(matches_segment("[a]", "[a]"));
        assert!(!matches_segment("[a]", "a"));
        assert!(matches_segment("data.json", "data.json"));
        assert!(!matches_segment("data.json", "data.json.bak"));
    }
}
