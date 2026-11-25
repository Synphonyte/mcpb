//! Reading a manifest back into a [`BundleSpec`].
//!
//! [`BundleSpec::manifest`] is the one writer; this is the reader that mirrors
//! it, so a manifest can be read, edited and written again - "same bundle, new
//! version". Paths come back as the manifest names them, relative to the
//! bundle's root; [`BundleSpec::in_dir`] joins them onto a directory, and
//! [`Bundle::spec`](crate::Bundle::spec) reads one out of a packed file.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::de::{Error as _, Unexpected};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Map, Value};

use crate::{
    Author, BundleFile, BundleSpec, Error, Executable, Icon, Localization, Platform,
    PlatformConfig, Prompt, Repository, UserConfig, UserConfigKind,
};

impl BundleSpec {
    /// The spec a manifest describes, with every path relative to the bundle's
    /// root, as the manifest names it.
    ///
    /// What the manifest does not carry is left empty: the architecture of a
    /// binary and the files the manifest does not name (helpers, resources and
    /// the per-locale translations), which
    /// [`Bundle::spec`](crate::Bundle::spec) fills in from a packed file.
    /// `manifest_version` is not carried either, since `manifest()` writes
    /// [`MANIFEST_VERSION`](crate::MANIFEST_VERSION).
    pub fn from_manifest(manifest: &Value) -> Result<Self, Error> {
        let root = manifest
            .as_object()
            .ok_or_else(|| Error::Manifest("it is not a JSON object".to_string()))?;

        let server = object(root.get("server"), "server")?;
        let config = object(server.get("mcp_config"), "server.mcp_config")?;
        let compatibility = object(root.get("compatibility"), "compatibility")?;

        // `manifest()` writes a binary server that starts its entry point, so a
        // manifest of another kind would come back out as something else.
        let kind = text(server.get("type"), "server.type")?;
        if kind != "binary" {
            return Err(Error::Manifest(format!(
                "server.type is {kind:?}: this crate packs binary servers only"
            )));
        }
        let entry_point = text(server.get("entry_point"), "server.entry_point")?;
        let command = text(config.get("command"), "server.mcp_config.command")?;
        if command != format!("${{__dirname}}/{entry_point}") {
            return Err(Error::Manifest(format!(
                "server.mcp_config.command {command:?} does not start the entry point: \
                 a binary bundle runs \"${{__dirname}}/{entry_point}\", and a platform \
                 that runs something else says so in its platform_overrides"
            )));
        }

        let version = text(root.get("version"), "version")?;
        let version = version.parse().map_err(|error| {
            Error::Manifest(format!("version {version:?} does not parse: {error}"))
        })?;

        Ok(Self {
            name: text(root.get("name"), "name")?,
            display_name: text_opt(root.get("display_name"), "display_name")?,
            version,
            description: text(root.get("description"), "description")?,
            long_description: text_opt(root.get("long_description"), "long_description")?,
            author: author(root.get("author"))?,
            repository: repository(root.get("repository"))?,
            homepage: text_opt(root.get("homepage"), "homepage")?,
            documentation: text_opt(root.get("documentation"), "documentation")?,
            support: text_opt(root.get("support"), "support")?,
            license: text_opt(root.get("license"), "license")?,
            keywords: texts(root.get("keywords"), "keywords")?,
            privacy_policies: texts(root.get("privacy_policies"), "privacy_policies")?,
            executables: executables(server, config, compatibility)?,
            extra_executables: Vec::new(),
            resources: Vec::new(),
            // A manifest names files, never directories: what a tree packed
            // reads back as the files it holds, next to the server.
            trees: Vec::new(),
            // A screenshot below the root keeps its place through a file that
            // puts it there; one at the root needs none.
            files: texts(root.get("screenshots"), "screenshots")?
                .into_iter()
                .filter(|name| name.contains('/'))
                .map(|name| BundleFile::data(&name, &name))
                .collect(),
            icon: text_opt(root.get("icon"), "icon")?.map(PathBuf::from),
            icons: icons(root.get("icons"))?,
            screenshots: texts(root.get("screenshots"), "screenshots")?
                .into_iter()
                .map(PathBuf::from)
                .collect(),
            tools: tools(root.get("tools"))?,
            tools_generated: flag(root.get("tools_generated"), "tools_generated")?,
            prompts: prompts(root.get("prompts"))?,
            prompts_generated: flag(root.get("prompts_generated"), "prompts_generated")?,
            localization: localization(root.get("localization"))?,
            user_config: user_config(root.get("user_config"))?,
            args: texts(config.get("args"), "server.mcp_config.args")?,
            env: env(config.get("env"), "server.mcp_config.env")?,
            platform_overrides: overrides(config.get("platform_overrides"))?,
            meta: meta(root.get("_meta"))?,
        })
    }

    /// The same spec with every path joined onto `dir`, such as the directory a
    /// bundle was extracted into.
    pub fn in_dir(&self, dir: impl AsRef<Path>) -> Self {
        let dir = dir.as_ref();
        let mut spec = self.clone();

        for executable in spec.executables.values_mut() {
            executable.path = dir.join(&executable.path);
        }
        spec.extra_executables = self
            .extra_executables
            .iter()
            .map(|path| dir.join(path))
            .collect();
        spec.resources = self.resources.iter().map(|path| dir.join(path)).collect();
        for (tree, path) in spec.trees.iter_mut().zip(&self.trees) {
            tree.path = dir.join(&path.path);
        }
        for (file, path) in spec.files.iter_mut().zip(&self.files) {
            file.path = dir.join(&path.path);
        }
        spec.icon = self.icon.as_deref().map(|path| dir.join(path));
        for (icon, path) in spec.icons.iter_mut().zip(&self.icons) {
            icon.path = dir.join(&path.path);
        }
        spec.screenshots = self.screenshots.iter().map(|path| dir.join(path)).collect();
        if let Some(localization) = &mut spec.localization {
            for path in localization.locales.values_mut() {
                *path = dir.join(&*path);
            }
        }

        spec
    }
}

impl Serialize for BundleSpec {
    /// The manifest, as [`BundleSpec::manifest`] writes it.
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.manifest().serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for BundleSpec {
    /// The manifest, read as [`BundleSpec::from_manifest`] reads it.
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let manifest = Value::deserialize(deserializer)?;
        Self::from_manifest(&manifest).map_err(D::Error::custom)
    }
}

impl Serialize for Platform {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for Platform {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let name = String::deserialize(deserializer)?;
        Self::parse(&name).ok_or_else(|| {
            D::Error::invalid_value(
                Unexpected::Str(&name),
                &"one of \"darwin\", \"linux\" and \"win32\"",
            )
        })
    }
}

impl UserConfigKind {
    /// From what the manifest writes: `string`, `number`, `boolean`, `directory`
    /// or `file`.
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "string" => Some(Self::String),
            "number" => Some(Self::Number),
            "boolean" => Some(Self::Boolean),
            "directory" => Some(Self::Directory),
            "file" => Some(Self::File),
            _ => None,
        }
    }
}

fn author(value: Option<&Value>) -> Result<Author, Error> {
    let Some(value) = value else {
        return Ok(Author::default());
    };
    let author = object(Some(value), "author")?;
    Ok(Author {
        name: text(author.get("name"), "author.name")?,
        email: text_opt(author.get("email"), "author.email")?,
        url: text_opt(author.get("url"), "author.url")?,
    })
}

fn repository(value: Option<&Value>) -> Result<Option<Repository>, Error> {
    let Some(value) = value else {
        return Ok(None);
    };
    let repository = object(Some(value), "repository")?;
    Ok(Some(Repository {
        kind: text(repository.get("type"), "repository.type")?,
        url: text(repository.get("url"), "repository.url")?,
    }))
}

/// The bundle's binaries: every platform it runs on, and what that platform
/// starts - its own override, or the entry point.
fn executables(
    server: &Map<String, Value>,
    config: &Map<String, Value>,
    compatibility: &Map<String, Value>,
) -> Result<BTreeMap<Platform, Executable>, Error> {
    let entry_point = text(server.get("entry_point"), "server.entry_point")?;
    let overrides = overrides(config.get("platform_overrides"))?;
    if !entry_point.starts_with("server/") {
        return Err(Error::Manifest(format!(
            "server.entry_point {entry_point:?} is not in server/, where this crate writes \
             the binaries"
        )));
    }

    let mut executables = BTreeMap::new();
    let mut platforms = Vec::new();
    for name in texts(compatibility.get("platforms"), "compatibility.platforms")? {
        let platform = Platform::parse(&name)
            .ok_or_else(|| Error::Manifest(format!("{name:?} is no platform a bundle runs on")))?;
        platforms.push(platform);
    }
    if platforms.is_empty() {
        return Err(Error::Manifest(
            "compatibility.platforms names no platform".to_string(),
        ));
    }

    for platform in platforms {
        // A platform whose override starts a file of `server/` runs a binary
        // of its own. Any other command - `node`, a wrapper elsewhere in the
        // bundle - wraps the entry point, and stays in the override.
        let own = overrides
            .get(&platform)
            .and_then(|config| config.command.as_deref())
            .and_then(|command| command.strip_prefix("${__dirname}/"))
            .filter(|path| path.starts_with("server/"));
        let path = own.unwrap_or(&entry_point).to_string();

        // The binary keeps its place under `server/`, subdirectory and all. The
        // manifest spells Windows' binary with its `.exe`; the spec leaves the
        // suffix to the writer.
        let mut name = path["server/".len()..].to_string();
        if platform == Platform::Win32 {
            name = name.strip_suffix(".exe").unwrap_or(&name).to_string();
        }

        executables.insert(
            platform,
            Executable {
                path: PathBuf::from(&path),
                arch: None,
                name: Some(name),
            },
        );
    }

    Ok(executables)
}

fn overrides(value: Option<&Value>) -> Result<BTreeMap<Platform, PlatformConfig>, Error> {
    let Some(value) = value else {
        return Ok(BTreeMap::new());
    };
    let overrides = object(Some(value), "server.mcp_config.platform_overrides")?;

    let mut configs = BTreeMap::new();
    for (name, config) in overrides {
        let platform = Platform::parse(name).ok_or_else(|| {
            Error::Manifest(format!(
                "server.mcp_config.platform_overrides.{name} names no platform a bundle runs on"
            ))
        })?;
        let at = format!("server.mcp_config.platform_overrides.{name}");
        let config = object(Some(config), &at)?;
        configs.insert(
            platform,
            PlatformConfig {
                command: text_opt(config.get("command"), &format!("{at}.command"))?,
                args: match config.get("args") {
                    Some(args) => Some(texts(Some(args), &format!("{at}.args"))?),
                    None => None,
                },
                env: match config.get("env") {
                    Some(env) => Some(env_map(env, &format!("{at}.env"))?),
                    None => None,
                },
            },
        );
    }

    Ok(configs)
}

fn icons(value: Option<&Value>) -> Result<Vec<Icon>, Error> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let array = value
        .as_array()
        .ok_or_else(|| Error::Manifest("icons is not a list".to_string()))?;

    array
        .iter()
        .enumerate()
        .map(|(index, value)| {
            let at = format!("icons[{index}]");
            let icon = object(Some(value), &at)?;
            Ok(Icon {
                path: PathBuf::from(text(icon.get("src"), &format!("{at}.src"))?),
                size: text(icon.get("size"), &format!("{at}.size"))?,
                theme: text_opt(icon.get("theme"), &format!("{at}.theme"))?,
            })
        })
        .collect()
}

fn tools(value: Option<&Value>) -> Result<Vec<(String, String)>, Error> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let array = value
        .as_array()
        .ok_or_else(|| Error::Manifest("tools is not a list".to_string()))?;

    array
        .iter()
        .enumerate()
        .map(|(index, value)| {
            let at = format!("tools[{index}]");
            let tool = object(Some(value), &at)?;
            Ok((
                text(tool.get("name"), &format!("{at}.name"))?,
                text(tool.get("description"), &format!("{at}.description"))?,
            ))
        })
        .collect()
}

fn prompts(value: Option<&Value>) -> Result<Vec<Prompt>, Error> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let array = value
        .as_array()
        .ok_or_else(|| Error::Manifest("prompts is not a list".to_string()))?;

    array
        .iter()
        .enumerate()
        .map(|(index, value)| {
            let at = format!("prompts[{index}]");
            let prompt = object(Some(value), &at)?;
            Ok(Prompt {
                name: text(prompt.get("name"), &format!("{at}.name"))?,
                description: text_opt(prompt.get("description"), &format!("{at}.description"))?,
                arguments: texts(prompt.get("arguments"), &format!("{at}.arguments"))?,
                text: text(prompt.get("text"), &format!("{at}.text"))?,
            })
        })
        .collect()
}

fn localization(value: Option<&Value>) -> Result<Option<Localization>, Error> {
    let Some(value) = value else {
        return Ok(None);
    };
    let localization = object(Some(value), "localization")?;

    Ok(Some(Localization {
        default_locale: text(
            localization.get("default_locale"),
            "localization.default_locale",
        )?,
        // The manifest points at `mcpb-resources/${locale}.json` and names no
        // locale; the files themselves are what say which ones exist.
        locales: BTreeMap::new(),
    }))
}

fn user_config(value: Option<&Value>) -> Result<Vec<UserConfig>, Error> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let config = object(Some(value), "user_config")?;

    config
        .iter()
        .map(|(key, value)| {
            let at = format!("user_config.{key}");
            let entry = object(Some(value), &at)?;
            let kind = text(entry.get("type"), &format!("{at}.type"))?;
            let kind = UserConfigKind::parse(&kind).ok_or_else(|| {
                Error::Manifest(format!("{at}.type {kind:?} is no kind of user_config"))
            })?;

            Ok(UserConfig {
                key: key.clone(),
                kind,
                title: text(entry.get("title"), &format!("{at}.title"))?,
                description: text(entry.get("description"), &format!("{at}.description"))?,
                required: flag(entry.get("required"), &format!("{at}.required"))?,
                sensitive: flag(entry.get("sensitive"), &format!("{at}.sensitive"))?,
                multiple: flag(entry.get("multiple"), &format!("{at}.multiple"))?,
                default: entry.get("default").cloned(),
                min: entry.get("min").and_then(Value::as_f64),
                max: entry.get("max").and_then(Value::as_f64),
            })
        })
        .collect()
}

fn meta(value: Option<&Value>) -> Result<Map<String, Value>, Error> {
    let Some(value) = value else {
        return Ok(Map::new());
    };
    Ok(object(Some(value), "_meta")?.clone())
}

fn env(value: Option<&Value>, at: &str) -> Result<BTreeMap<String, String>, Error> {
    let Some(value) = value else {
        return Ok(BTreeMap::new());
    };
    env_map(value, at)
}

fn env_map(value: &Value, at: &str) -> Result<BTreeMap<String, String>, Error> {
    let object = object(Some(value), at)?;

    object
        .iter()
        .map(|(key, value)| {
            let value = text(Some(value), &format!("{at}.{key}"))?;
            Ok((key.clone(), value))
        })
        .collect()
}

fn object<'a>(value: Option<&'a Value>, at: &str) -> Result<&'a Map<String, Value>, Error> {
    match value {
        Some(Value::Object(object)) => Ok(object),
        Some(_) => Err(Error::Manifest(format!("{at} is no JSON object"))),
        None => Err(Error::Manifest(format!("there is no {at}"))),
    }
}

fn text(value: Option<&Value>, at: &str) -> Result<String, Error> {
    match value {
        Some(Value::String(text)) => Ok(text.clone()),
        Some(_) => Err(Error::Manifest(format!("{at} is no string"))),
        None => Err(Error::Manifest(format!("there is no {at}"))),
    }
}

fn text_opt(value: Option<&Value>, at: &str) -> Result<Option<String>, Error> {
    match value {
        Some(Value::String(text)) => Ok(Some(text.clone())),
        Some(Value::Null) | None => Ok(None),
        Some(_) => Err(Error::Manifest(format!("{at} is no string"))),
    }
}

fn texts(value: Option<&Value>, at: &str) -> Result<Vec<String>, Error> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    value
        .as_array()
        .ok_or_else(|| Error::Manifest(format!("{at} is no list")))?
        .iter()
        .enumerate()
        .map(|(index, value)| text(Some(value), &format!("{at}[{index}]")))
        .collect()
}

fn flag(value: Option<&Value>, at: &str) -> Result<bool, Error> {
    match value {
        Some(Value::Bool(flag)) => Ok(*flag),
        Some(_) => Err(Error::Manifest(format!("{at} is no true or false"))),
        None => Ok(false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_spec_in_a_directory_joins_every_path() {
        let spec = BundleSpec {
            name: "s".to_string(),
            version: "1.0.0".parse().unwrap(),
            description: "d".to_string(),
            author: Author::new("me"),
            executables: BTreeMap::from([(Platform::Linux, Executable::new("server/s", "x64"))]),
            extra_executables: vec!["server/helper".into()],
            resources: vec!["server/NOTICE".into()],
            icon: Some("icon.png".into()),
            icons: vec![Icon {
                path: "icon-dark.png".into(),
                size: "16x16".to_string(),
                theme: Some("dark".to_string()),
            }],
            screenshots: vec!["shot.png".into()],
            localization: Some(Localization {
                default_locale: "en-US".to_string(),
                locales: BTreeMap::from([("de-DE".to_string(), "de-DE.json".into())]),
            }),
            ..BundleSpec::default()
        };

        let joined = spec.in_dir("/bundle");
        let paths = [
            joined.executables[&Platform::Linux].path.clone(),
            joined.extra_executables[0].clone(),
            joined.resources[0].clone(),
            joined.icon.clone().unwrap(),
            joined.icons[0].path.clone(),
            joined.screenshots[0].clone(),
            joined.localization.as_ref().unwrap().locales["de-DE"].clone(),
        ];

        for path in paths {
            assert!(path.starts_with("/bundle"), "{path:?}");
        }
        assert_eq!(
            joined.localization.as_ref().unwrap().locales["de-DE"],
            Path::new("/bundle/de-DE.json")
        );
        // The spec itself is left as it was.
        assert_eq!(
            spec.executables[&Platform::Linux].path,
            Path::new("server/s")
        );
    }

    /// A binary manifest with `entry_point`, started as it says.
    fn binary(entry_point: &str) -> Value {
        serde_json::json!({
            "manifest_version": "0.3",
            "name": "s",
            "version": "1.0.0",
            "description": "d",
            "author": { "name": "me" },
            "server": {
                "type": "binary",
                "entry_point": entry_point,
                "mcp_config": { "command": format!("${{__dirname}}/{entry_point}") },
            },
            "compatibility": { "platforms": ["darwin"] },
        })
    }

    #[test]
    fn only_a_binary_server_is_read() {
        let mut manifest = binary("server/index.js");
        manifest["server"]["type"] = "node".into();
        manifest["server"]["mcp_config"]["command"] = "node".into();

        let error = BundleSpec::from_manifest(&manifest).unwrap_err();
        assert!(error.to_string().contains("binary servers only"), "{error}");
    }

    #[test]
    fn a_command_other_than_the_entry_point_is_refused() {
        let mut manifest = binary("server/s");
        manifest["server"]["mcp_config"]["command"] = "${__dirname}/server/other".into();

        let error = BundleSpec::from_manifest(&manifest).unwrap_err();
        assert!(
            error.to_string().contains("does not start the entry point"),
            "{error}"
        );
    }

    #[test]
    fn an_entry_point_keeps_its_directory_under_server() {
        let spec = BundleSpec::from_manifest(&binary("server/bin/s")).unwrap();
        assert_eq!(spec.entry_point().as_deref(), Some("server/bin/s"));
        assert_eq!(spec.manifest(), {
            let mut manifest = binary("server/bin/s");
            manifest["display_name"] = "s".into();
            manifest["server"]["mcp_config"]["args"] = serde_json::json!([]);
            manifest["server"]["mcp_config"]["env"] = serde_json::json!({});
            manifest["tools"] = serde_json::json!([]);
            manifest
        });
        assert_eq!(spec.validate(), Ok(()));

        let error = BundleSpec::from_manifest(&binary("bin/s")).unwrap_err();
        assert!(error.to_string().contains("is not in server/"), "{error}");
    }

    #[test]
    fn an_override_that_wraps_the_server_keeps_its_binary() {
        let mut manifest = binary("server/s");
        manifest["compatibility"]["platforms"] = serde_json::json!(["darwin", "linux"]);
        manifest["server"]["mcp_config"]["platform_overrides"] =
            serde_json::json!({ "linux": { "command": "/usr/bin/env" } });

        let spec = BundleSpec::from_manifest(&manifest).unwrap();
        assert_eq!(
            spec.executables[&Platform::Linux].path,
            Path::new("server/s")
        );
        assert_eq!(
            spec.manifest()["server"]["mcp_config"]["platform_overrides"],
            serde_json::json!({ "linux": { "command": "/usr/bin/env" } })
        );
    }

    #[test]
    fn a_nested_screenshot_reads_back_in_its_place() {
        let mut manifest = binary("server/s");
        manifest["screenshots"] = serde_json::json!(["shot.png", "assets/wide.png"]);

        let spec = BundleSpec::from_manifest(&manifest)
            .unwrap()
            .in_dir("/unpacked");
        assert_eq!(
            spec.manifest()["screenshots"],
            serde_json::json!(["shot.png", "assets/wide.png"])
        );
    }

    #[test]
    fn what_a_manifest_is_missing_is_named() {
        let error = BundleSpec::from_manifest(&serde_json::json!({})).unwrap_err();
        assert!(error.to_string().contains("there is no server"), "{error}");
    }
}
