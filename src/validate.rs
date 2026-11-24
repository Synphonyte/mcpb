//! What a [`BundleSpec`] has to get right before clients would accept it.

use std::collections::{BTreeMap, HashSet};

use serde_json::Value;

use crate::{BundleSpec, Platform, PlatformConfig, UserConfigKind};

impl BundleSpec {
    /// Everything wrong with this spec, or `Ok` when a client would accept it.
    ///
    /// Every problem is reported, not just the first, so one call tells the
    /// whole story. The file system is not consulted: whether the files exist
    /// and share names is checked by [`Self::pack`].
    pub fn validate(&self) -> Result<(), Vec<String>> {
        let mut problems = Vec::new();

        if !is_name(&self.name) {
            problems.push(format!(
                "name {:?} may only hold ASCII letters, digits, '-', '_' and '.'",
                self.name
            ));
        }
        if let Err(error) = semver::Version::parse(&self.version) {
            problems.push(format!(
                "version {:?} is not a semantic version: {error}",
                self.version
            ));
        }
        if self.description.trim().is_empty() {
            problems.push("description is empty".to_string());
        }
        if self.author.name.trim().is_empty() {
            problems.push("the author has no name".to_string());
        }
        if let Some(email) = &self.author.email {
            if !is_email(email) {
                problems.push(format!("author email {email:?} is not an email address"));
            }
        }
        if let Some(url) = &self.author.url {
            if !is_uri(url) {
                problems.push(format!("author url {url:?} is not a URL"));
            }
        }
        if self
            .long_description
            .as_deref()
            .is_some_and(|text| text.trim().is_empty())
        {
            problems.push("long_description is empty".to_string());
        }
        if let Some(repository) = &self.repository {
            if repository.kind.trim().is_empty() {
                problems.push("the repository has no type, such as \"git\"".to_string());
            }
            if !is_uri(&repository.url) {
                problems.push(format!("repository url {:?} is not a URL", repository.url));
            }
        }
        for (what, url) in [
            ("homepage", &self.homepage),
            ("documentation", &self.documentation),
            ("support", &self.support),
        ] {
            if let Some(url) = url.as_ref().filter(|url| !is_uri(url)) {
                problems.push(format!("{what} {url:?} is not a URL"));
            }
        }
        if self
            .license
            .as_deref()
            .is_some_and(|license| license.trim().is_empty())
        {
            problems.push("license is empty".to_string());
        }
        let mut keyword_names = HashSet::new();
        for keyword in &self.keywords {
            if keyword.trim().is_empty() {
                problems.push("a keyword is empty".to_string());
            } else if !keyword_names.insert(keyword.as_str()) {
                problems.push(format!("two keywords are {keyword:?}"));
            }
        }
        for policy in &self.privacy_policies {
            if !is_uri(policy) {
                problems.push(format!(
                    "privacy_policies has {policy:?}, which is not a URL"
                ));
            }
        }
        if self
            .display_name
            .as_deref()
            .is_some_and(|name| name.trim().is_empty())
        {
            problems.push("display_name is empty".to_string());
        }
        let mut config_keys = HashSet::new();
        for entry in &self.user_config {
            if !is_config_key(&entry.key) {
                problems.push(format!(
                    "user_config key {:?} may only hold ASCII letters, digits, '_', '-' and '.'",
                    entry.key
                ));
            } else if !config_keys.insert(entry.key.as_str()) {
                problems.push(format!("two user_config entries are named {:?}", entry.key));
            }
            if entry.title.trim().is_empty() {
                problems.push(format!("user_config {:?} has no title", entry.key));
            }
            if entry.description.trim().is_empty() {
                problems.push(format!("user_config {:?} has no description", entry.key));
            }
            if entry.kind != UserConfigKind::Number && (entry.min.is_some() || entry.max.is_some())
            {
                problems.push(format!(
                    "user_config {:?} sets min or max, which only a number takes",
                    entry.key
                ));
            }
            if let (Some(min), Some(max)) = (entry.min, entry.max) {
                if min > max {
                    problems.push(format!(
                        "user_config {:?} has min {min} above max {max}",
                        entry.key
                    ));
                }
            }
            if let Some(default) = &entry.default {
                if !default_fits(entry.kind, entry.multiple, default) {
                    problems.push(format!(
                        "user_config {:?} has a default that does not fit its type",
                        entry.key
                    ));
                }
            }
        }

        let mut tool_names = HashSet::new();
        for (name, description) in &self.tools {
            if name.trim().is_empty() {
                problems.push("a tool has no name".to_string());
            } else if !tool_names.insert(name.as_str()) {
                problems.push(format!("two tools are named {name:?}"));
            }
            if description.trim().is_empty() {
                problems.push(format!("the tool {name:?} has no description"));
            }
        }
        let mut prompt_names = HashSet::new();
        for prompt in &self.prompts {
            if prompt.name.trim().is_empty() {
                problems.push("a prompt needs a name".to_string());
            } else if !prompt_names.insert(prompt.name.as_str()) {
                problems.push(format!("two prompts are named {:?}", prompt.name));
            }
            if prompt.text.trim().is_empty() {
                problems.push(format!("the prompt {:?} has no text", prompt.name));
            }
            let mut argument_names = HashSet::new();
            for argument in &prompt.arguments {
                if argument.trim().is_empty() {
                    problems.push(format!(
                        "the prompt {:?} has an argument with no name",
                        prompt.name
                    ));
                } else if !argument_names.insert(argument.as_str()) {
                    problems.push(format!(
                        "the prompt {:?} has two arguments named {argument:?}",
                        prompt.name
                    ));
                }
            }
            let arguments: Vec<&str> = prompt
                .arguments
                .iter()
                .map(String::as_str)
                .filter(|argument| !argument.trim().is_empty())
                .collect();
            for variable in variables(&prompt.text) {
                let argument = variable.strip_prefix("arguments.");
                if !argument.is_some_and(|argument| arguments.contains(&argument)) {
                    problems.push(format!(
                        "the prompt {:?} uses ${{{variable}}}: only ${{arguments.<argument>}} of its own arguments {} are substituted",
                        prompt.name,
                        names(&arguments),
                    ));
                }
            }
        }

        for icon in &self.icons {
            if !is_size(&icon.size) {
                problems.push(format!(
                    "icon size {:?} is not WIDTHxHEIGHT, such as \"128x128\"",
                    icon.size
                ));
            }
            if icon
                .theme
                .as_deref()
                .is_some_and(|theme| theme.trim().is_empty())
            {
                problems.push("an icon has an empty theme".to_string());
            }
        }

        if let Some(localization) = &self.localization {
            if !is_locale(&localization.default_locale) {
                problems.push(format!(
                    "default_locale {:?} is not a language code such as \"en-US\"",
                    localization.default_locale
                ));
            }
            if localization.locales.is_empty() {
                problems.push(
                    "localization names no locale: clients would find no translated strings"
                        .to_string(),
                );
            }
            for locale in localization.locales.keys() {
                if !is_locale(locale) {
                    problems.push(format!(
                        "locale {locale:?} is not a language code such as \"de-DE\""
                    ));
                }
            }
        }

        for (namespace, metadata) in &self.meta {
            if namespace.trim().is_empty() {
                problems.push("a _meta namespace is empty".to_string());
            }
            if !metadata.is_object() {
                problems.push(format!("_meta.{namespace} holds no object"));
            }
        }

        let user_config = self.user_config_keys();
        for (index, arg) in self.args.iter().enumerate() {
            problems.extend(check_variables(
                &format!("args[{index}]"),
                arg,
                &user_config,
            ));
        }
        problems.extend(check_env(&self.env, "env", &user_config));
        for (platform, config) in &self.platform_overrides {
            // The manifest names this one platform, so no client would ever read
            // another platform's override.
            if *platform != self.platform {
                problems.push(format!(
                    "platform_overrides.{} does not apply: the bundle runs on {} alone",
                    platform.as_str(),
                    self.platform.as_str()
                ));
            }
            problems.extend(check_override(*platform, config, &user_config));
        }

        if problems.is_empty() {
            Ok(())
        } else {
            Err(problems)
        }
    }

    /// What the manifest's `user_config` declares, which `${user_config.<key>}`
    /// may refer to.
    pub(crate) fn user_config_keys(&self) -> Vec<&str> {
        self.user_config
            .iter()
            .map(|entry| entry.key.as_str())
            .collect()
    }
}

fn check_override(
    platform: Platform,
    config: &PlatformConfig,
    user_config: &[&str],
) -> Vec<String> {
    let mut problems = Vec::new();
    let at = |what: &str| format!("platform_overrides.{}.{what}", platform.as_str());

    if let Some(command) = &config.command {
        problems.extend(check_variables(&at("command"), command, user_config));
    }
    if let Some(args) = &config.args {
        for (index, arg) in args.iter().enumerate() {
            let what = at(&format!("args[{index}]"));
            problems.extend(check_variables(&what, arg, user_config));
        }
    }
    if let Some(env) = &config.env {
        problems.extend(check_env(env, &at("env"), user_config));
    }

    problems
}

fn check_env(env: &BTreeMap<String, String>, at: &str, user_config: &[&str]) -> Vec<String> {
    let mut problems = Vec::new();
    for (key, value) in env {
        if !is_env_key(key) {
            problems.push(format!(
                "{at} has {key:?}, which is not an environment variable name"
            ));
        }
        problems.extend(check_variables(&format!("{at}.{key}"), value, user_config));
    }
    problems
}

/// Refuses `${...}` that a client would not substitute, and
/// `${user_config.<key>}` of a key the manifest does not declare.
fn check_variables(what: &str, value: &str, user_config: &[&str]) -> Vec<String> {
    let mut problems = Vec::new();
    for variable in variables(value) {
        if SUBSTITUTED.contains(&variable) {
            continue;
        }

        match variable.strip_prefix("user_config.") {
            Some(key) if user_config.contains(&key) => {}
            Some(key) => problems.push(format!(
                "{what} uses ${{{variable}}}, but the manifest declares no {key:?}; it asks for {}",
                names(user_config)
            )),
            None => problems.push(format!(
                "{what} uses ${{{variable}}}, which clients do not substitute: {SUBSTITUTED_LIST}"
            )),
        }
    }
    problems
}

/// The variables clients substitute, besides `${user_config.<key>}` of the keys
/// the manifest declares.
const SUBSTITUTED: [&str; 7] = [
    "__dirname",
    "HOME",
    "DOCUMENTS",
    "DOWNLOADS",
    "DESKTOP",
    "pathSeparator",
    "/",
];

/// What the messages say is substituted.
const SUBSTITUTED_LIST: &str = "${__dirname}, ${HOME}, ${DOCUMENTS}, ${DOWNLOADS}, ${DESKTOP}, ${pathSeparator}, ${/} and ${user_config.<key>}";

/// What stands between `${` and `}` in a value.
fn variables(value: &str) -> Vec<&str> {
    let mut variables = Vec::new();
    let mut rest = value;
    while let Some(start) = rest.find("${") {
        let after = &rest[start + 2..];
        let Some(end) = after.find('}') else {
            break;
        };
        variables.push(&after[..end]);
        rest = &after[end + 1..];
    }
    variables
}

/// The names, quoted for a message.
fn names<S: AsRef<str>>(names: &[S]) -> String {
    if names.is_empty() {
        "none".to_string()
    } else {
        names
            .iter()
            .map(|name| format!("{:?}", name.as_ref()))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// Whether the key can name a `user_config` entry and the variable that refers
/// to it.
fn is_config_key(key: &str) -> bool {
    !key.is_empty()
        && key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
}

/// Whether the default value fits the entry: one value of its type, or a list of
/// them when several may be given.
fn default_fits(kind: UserConfigKind, multiple: bool, default: &Value) -> bool {
    let one_fits = |value: &Value| match kind {
        UserConfigKind::String | UserConfigKind::Directory | UserConfigKind::File => {
            value.is_string()
        }
        UserConfigKind::Number => value.is_number(),
        UserConfigKind::Boolean => value.is_boolean(),
    };

    if multiple {
        default
            .as_array()
            .is_some_and(|values| values.iter().all(one_fits))
    } else {
        one_fits(default)
    }
}

/// Whether the text looks like a URL: a scheme and no whitespace.
fn is_uri(value: &str) -> bool {
    !value.chars().any(char::is_whitespace)
        && value
            .split_once(':')
            .is_some_and(|(scheme, rest)| !scheme.is_empty() && !rest.is_empty())
}

/// Whether the text looks like an email address.
fn is_email(value: &str) -> bool {
    match value.split_once('@') {
        Some((local, domain)) => {
            !local.is_empty()
                && !domain.is_empty()
                && !domain.contains('@')
                && !value.chars().any(char::is_whitespace)
        }
        None => false,
    }
}

/// Whether the name can be a bundle's, an executable's and a file name's own.
fn is_name(name: &str) -> bool {
    // `.` and `..` name directories, not files.
    !matches!(name, "" | "." | "..")
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// Whether the size reads `WIDTHxHEIGHT`.
fn is_size(size: &str) -> bool {
    match size.split_once('x') {
        Some((width, height)) => {
            !width.is_empty()
                && width.chars().all(|c| c.is_ascii_digit())
                && !height.is_empty()
                && height.chars().all(|c| c.is_ascii_digit())
        }
        None => false,
    }
}

/// Whether the key is one a server's environment can hold.
fn is_env_key(key: &str) -> bool {
    let mut chars = key.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Whether the locale reads like a BCP 47 language tag, as the manifest format
/// asks for: `en`, `en-US`, `zh-Hans`, `de-DE-1996`.
fn is_locale(locale: &str) -> bool {
    let mut parts = locale.split('-');
    let language = parts.next().unwrap_or_default();

    (2..=8).contains(&language.len())
        && language.chars().all(|c| c.is_ascii_alphabetic())
        && parts.all(|part| {
            (1..=8).contains(&part.len()) && part.chars().all(|c| c.is_ascii_alphanumeric())
        })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    use serde_json::json;

    use super::*;
    use crate::{Author, Icon, Localization, Prompt, Repository, UserConfig};

    fn spec() -> BundleSpec {
        BundleSpec {
            name: "test-server".to_string(),
            version: "1.2.3".to_string(),
            description: "A test server.".to_string(),
            author: Author::new("Test"),
            platform: Platform::Linux,
            executable: PathBuf::from("server-binary"),
            tools: vec![("whoami".to_string(), "Who the caller is.".to_string())],
            user_config: vec![UserConfig::directory(
                "project_dir",
                "Project directory",
                "The directory the server works in.",
            )],
            args: vec!["${user_config.project_dir}".to_string()],
            ..BundleSpec::default()
        }
    }

    /// The problems of a spec, which must be reported one per line.
    fn problems(spec: &BundleSpec) -> Vec<String> {
        spec.validate().unwrap_err()
    }

    #[test]
    fn a_plain_spec_is_valid() {
        assert_eq!(spec().validate(), Ok(()));
    }

    #[test]
    fn the_name_has_to_be_usable_as_a_file_name() {
        for name in ["", ".", "..", "my server", "my/server", "my-server!"] {
            let mut spec = spec();
            spec.name = name.to_string();
            assert!(
                problems(&spec)
                    .iter()
                    .any(|problem| problem.contains("name")),
                "{name:?}"
            );
        }

        for name in ["my-server", "my_server", "my.server", "Server2"] {
            let mut spec = spec();
            spec.name = name.to_string();
            assert_eq!(spec.validate(), Ok(()), "{name:?}");
        }
    }

    #[test]
    fn the_version_has_to_be_a_semantic_version() {
        for version in ["", "1.2", "1.2.3.4", "v1.2.3", "1.2.x"] {
            let mut spec = spec();
            spec.version = version.to_string();
            assert!(
                problems(&spec)
                    .iter()
                    .any(|problem| problem.contains("semantic version")),
                "{version:?}"
            );
        }

        for version in ["1.2.3", "0.1.0", "1.0.0-rc.1", "1.0.0+build.5"] {
            let mut spec = spec();
            spec.version = version.to_string();
            assert_eq!(spec.validate(), Ok(()), "{version:?}");
        }
    }

    #[test]
    fn what_clients_show_has_to_say_something() {
        let mut spec = spec();
        spec.description = " ".to_string();
        spec.author = Author::default();
        spec.display_name = Some(String::new());
        spec.long_description = Some("\n".to_string());
        spec.license = Some(String::new());
        spec.user_config = vec![UserConfig {
            title: String::new(),
            description: String::new(),
            ..UserConfig::directory("project_dir", "Project directory", "Where it works.")
        }];

        assert_eq!(
            problems(&spec),
            [
                "description is empty",
                "the author has no name",
                "long_description is empty",
                "license is empty",
                "display_name is empty",
                "user_config \"project_dir\" has no title",
                "user_config \"project_dir\" has no description",
            ]
        );
    }

    #[test]
    fn links_and_the_author_have_to_look_like_what_they_claim() {
        let mut spec = spec();
        spec.author.email = Some("maccesch.synphonyte.com".to_string());
        spec.author.url = Some("https://synphonyte.com".to_string());
        spec.repository = Some(Repository {
            kind: String::new(),
            url: "github.com/Synphonyte/mcpb".to_string(),
        });
        spec.homepage = Some("https://synphonyte.com/mcpb".to_string());
        spec.documentation = Some("https://docs.rs/mcpb".to_string());
        spec.support = Some("mailto:hello@synphonyte.com".to_string());
        spec.privacy_policies = vec![
            "https://synphonyte.com/privacy".to_string(),
            "nope".to_string(),
        ];

        assert_eq!(
            problems(&spec),
            [
                "author email \"maccesch.synphonyte.com\" is not an email address",
                "the repository has no type, such as \"git\"",
                "repository url \"github.com/Synphonyte/mcpb\" is not a URL",
                "privacy_policies has \"nope\", which is not a URL",
            ]
        );

        spec.author.email = Some("maccesch@synphonyte.com".to_string());
        spec.repository = Some(Repository::git("https://github.com/Synphonyte/mcpb"));
        spec.privacy_policies = vec!["https://synphonyte.com/privacy".to_string()];
        assert_eq!(spec.validate(), Ok(()));
    }

    #[test]
    fn keywords_and_meta_are_read_once() {
        let mut spec = spec();
        spec.keywords = vec!["mcp".to_string(), "mcp".to_string(), String::new()];
        spec.meta = serde_json::Map::from_iter([
            (
                "com.microsoft.windows".to_string(),
                json!({ "package_family_name": "Mcpb" }),
            ),
            (String::new(), json!({})),
            ("de.synphonyte".to_string(), json!("not an object")),
        ]);

        assert_eq!(
            problems(&spec),
            [
                "two keywords are \"mcp\"",
                "a keyword is empty",
                "a _meta namespace is empty",
                "_meta.de.synphonyte holds no object",
            ]
        );
    }

    #[test]
    fn user_config_entries_have_to_fit_their_type() {
        let mut spec = spec();
        spec.user_config = vec![
            UserConfig {
                min: Some(1.0),
                ..UserConfig::directory("project_dir", "Project", "Where it works.")
            },
            UserConfig {
                key: "project dir".to_string(),
                ..UserConfig::string("ignored", "Title", "Description")
            },
            UserConfig {
                default: Some(json!("ten")),
                ..UserConfig::number("limit", "Limit", "How many.")
            },
            UserConfig {
                min: Some(100.0),
                max: Some(1.0),
                ..UserConfig::number("range", "Range", "Between.")
            },
            UserConfig {
                default: Some(json!([1, 2])),
                multiple: true,
                ..UserConfig::string("tags", "Tags", "What it is about.")
            },
            UserConfig::boolean("project_dir", "Again", "A second entry with the same key."),
        ];

        assert_eq!(
            problems(&spec),
            [
                "user_config \"project_dir\" sets min or max, which only a number takes",
                "user_config key \"project dir\" may only hold ASCII letters, digits, '_', '-' and '.'",
                "user_config \"limit\" has a default that does not fit its type",
                "user_config \"range\" has min 100 above max 1",
                "user_config \"tags\" has a default that does not fit its type",
                "two user_config entries are named \"project_dir\"",
            ]
        );

        spec.args.clear();
        spec.user_config = vec![UserConfig {
            multiple: true,
            default: Some(json!(["a", "b"])),
            ..UserConfig::string("tags", "Tags", "What it is about.")
        }];
        assert_eq!(spec.validate(), Ok(()));
    }

    #[test]
    fn tools_and_prompts_have_to_be_readable() {
        let mut spec = spec();
        spec.tools = vec![("whoami".to_string(), String::new())];
        spec.prompts = vec![Prompt {
            name: "review".to_string(),
            description: None,
            arguments: vec!["path".to_string(), String::new()],
            text: "Review ${arguments.path} and ${arguments.file}.".to_string(),
        }];

        assert_eq!(
            problems(&spec),
            [
                "the tool \"whoami\" has no description",
                "the prompt \"review\" has an argument with no name",
                "the prompt \"review\" uses ${arguments.file}: only ${arguments.<argument>} of its own arguments \"path\" are substituted",
            ]
        );
    }

    #[test]
    fn tools_prompts_and_arguments_are_named_once() {
        let mut spec = spec();
        spec.tools = vec![
            ("whoami".to_string(), "Who the caller is.".to_string()),
            ("whoami".to_string(), "Who the caller is.".to_string()),
        ];
        spec.prompts = vec![
            Prompt {
                name: "review".to_string(),
                description: None,
                arguments: vec!["path".to_string(), "path".to_string()],
                text: "Review ${arguments.path}.".to_string(),
            },
            Prompt {
                name: "review".to_string(),
                description: None,
                arguments: Vec::new(),
                text: "Review again.".to_string(),
            },
        ];

        assert_eq!(
            problems(&spec),
            [
                "two tools are named \"whoami\"",
                "the prompt \"review\" has two arguments named \"path\"",
                "two prompts are named \"review\"",
            ]
        );
    }

    #[test]
    fn env_values_may_only_use_variables_clients_substitute() {
        let mut spec = spec();
        spec.env
            .insert("API_KEY".to_string(), "${user_config.api_key}".to_string());
        spec.env
            .insert("API-URL".to_string(), "${base_url}".to_string());

        assert_eq!(
            problems(&spec),
            [
                "env has \"API-URL\", which is not an environment variable name",
                "env.API-URL uses ${base_url}, which clients do not substitute: ${__dirname}, ${HOME}, ${DOCUMENTS}, ${DOWNLOADS}, ${DESKTOP}, ${pathSeparator}, ${/} and ${user_config.<key>}",
                "env.API_KEY uses ${user_config.api_key}, but the manifest declares no \"api_key\"; it asks for \"project_dir\"",
            ]
        );

        spec.env.clear();
        spec.env.insert(
            "PROJECT".to_string(),
            "${user_config.project_dir}".to_string(),
        );
        spec.env.insert(
            "HOME_DIR".to_string(),
            "${HOME}/${__dirname}/../x".to_string(),
        );
        assert_eq!(spec.validate(), Ok(()));
    }

    #[test]
    fn platform_overrides_are_checked_like_the_rest() {
        let mut spec = spec();
        spec.platform = Platform::Win32;
        spec.platform_overrides.insert(
            Platform::Win32,
            PlatformConfig {
                command: None,
                args: Some(vec!["${user_config.windows_dir}".to_string()]),
                env: Some(BTreeMap::from([
                    ("PATH".to_string(), "${__dirname}\\bin".to_string()),
                    ("BAD KEY".to_string(), String::new()),
                ])),
            },
        );

        assert_eq!(
            problems(&spec),
            [
                "platform_overrides.win32.args[0] uses ${user_config.windows_dir}, but the manifest declares no \"windows_dir\"; it asks for \"project_dir\"",
                "platform_overrides.win32.env has \"BAD KEY\", which is not an environment variable name",
            ]
        );
    }

    #[test]
    fn an_override_for_another_platform_does_not_apply() {
        let mut spec = spec();
        spec.platform_overrides.insert(
            Platform::Win32,
            PlatformConfig {
                args: Some(vec!["--windows".to_string()]),
                ..PlatformConfig::default()
            },
        );

        assert_eq!(
            problems(&spec),
            ["platform_overrides.win32 does not apply: the bundle runs on linux alone"]
        );
    }

    #[test]
    fn icons_have_to_say_the_size_they_are_drawn_at() {
        let mut spec = spec();
        spec.icons = vec![
            Icon {
                path: "icon.png".into(),
                size: "128x".to_string(),
                theme: Some(String::new()),
            },
            Icon {
                path: "other.png".into(),
                size: "128x128".to_string(),
                theme: None,
            },
        ];

        assert_eq!(
            problems(&spec),
            [
                "icon size \"128x\" is not WIDTHxHEIGHT, such as \"128x128\"",
                "an icon has an empty theme",
            ]
        );
    }

    #[test]
    fn translations_have_to_name_their_locales() {
        let mut spec = spec();
        spec.localization = Some(Localization {
            default_locale: "en_US".to_string(),
            locales: BTreeMap::new(),
        });
        assert_eq!(
            problems(&spec),
            [
                "default_locale \"en_US\" is not a language code such as \"en-US\"",
                "localization names no locale: clients would find no translated strings",
            ]
        );

        spec.localization = Some(Localization {
            default_locale: "en-US".to_string(),
            locales: BTreeMap::from([("de-DE".to_string(), "de.json".into())]),
        });
        assert_eq!(spec.validate(), Ok(()));
    }

    #[test]
    fn locales_read_like_language_tags() {
        for locale in ["en", "en-US", "zh-Hans", "de-DE-1996", "es-UY", "english"] {
            assert!(is_locale(locale), "{locale}");
        }
        for locale in [
            "",
            "e",
            "e2",
            "en_US",
            "en-US!",
            "en--US",
            "englishlanguage",
        ] {
            assert!(!is_locale(locale), "{locale}");
        }
    }

    #[test]
    fn variables_are_read_wherever_they_stand() {
        assert_eq!(variables("plain"), Vec::<&str>::new());
        assert_eq!(
            variables("${__dirname}/x-${user_config.a}"),
            ["__dirname", "user_config.a"]
        );
        assert_eq!(variables("${unclosed"), Vec::<&str>::new());
        assert_eq!(variables("${"), Vec::<&str>::new());
    }
}
