//! The manifests this crate writes, checked against the manifest format's own
//! JSON schema - so "clients accept this" is not only what our own tests expect.
//!
//! The schema is vendored from the MCP Bundle repository; see
//! `tests/schemas/README.md`. `jsonschema` is a dev-dependency: nothing in the
//! published crate uses it.

use std::collections::BTreeMap;

use mcpb::{
    Author, BundleSpec, Executable, Icon, Localization, Platform, PlatformConfig, Prompt,
    Repository, UserConfig,
};
use serde_json::{Value, json};

const SCHEMA: &str = include_str!("schemas/mcpb-manifest-v0.3.schema.json");

/// What the schema says is wrong with the manifest, if anything.
fn problems(manifest: &Value) -> Vec<String> {
    let schema: Value = serde_json::from_str(SCHEMA).expect("the vendored schema is JSON");
    let validator = jsonschema::validator_for(&schema).expect("the vendored schema is a schema");

    validator
        .iter_errors(manifest)
        .map(|error| format!("{error} (at {})", error.instance_path()))
        .collect()
}

#[track_caller]
fn assert_fits(spec: &BundleSpec) {
    assert_eq!(
        spec.validate(),
        Ok(()),
        "the test spec itself is not what the crate accepts"
    );

    let manifest = spec.manifest();
    let problems = problems(&manifest);
    assert!(
        problems.is_empty(),
        "{:?} writes a manifest the schema refuses:\n{}\n\n{}",
        spec.name,
        problems.join("\n"),
        serde_json::to_string_pretty(&manifest).expect("a JSON value serializes")
    );
}

/// The few fields a bundle cannot do without.
fn minimal() -> BundleSpec {
    BundleSpec {
        name: "schema-server".to_string(),
        version: "1.2.3".to_string(),
        description: "A server the schema checks.".to_string(),
        author: Author::new("Test"),
        executables: BTreeMap::from([(Platform::Linux, Executable::new("server-binary", "x64"))]),
        ..BundleSpec::default()
    }
}

#[test]
fn the_smallest_spec_fits_the_schema() {
    assert_fits(&minimal());
}

#[test]
fn a_spec_that_uses_every_field_fits_the_schema() {
    let mut spec = minimal();
    spec.display_name = Some("Schema Server".to_string());
    spec.long_description = Some("A longer text about the server.".to_string());
    spec.author = Author {
        name: "Test".to_string(),
        email: Some("test@example.com".to_string()),
        url: Some("https://example.com".to_string()),
    };
    spec.repository = Some(Repository::git("https://github.com/Synphonyte/mcpb"));
    spec.homepage = Some("https://example.com".to_string());
    spec.documentation = Some("https://docs.example.com".to_string());
    spec.support = Some("https://example.com/issues".to_string());
    spec.license = Some("MIT".to_string());
    spec.keywords = vec!["mcp".to_string(), "bundle".to_string()];
    spec.privacy_policies = vec!["https://example.com/privacy".to_string()];
    spec.icon = Some("icon.png".into());
    spec.icons = vec![
        Icon {
            path: "icon-16-light.png".into(),
            size: "16x16".to_string(),
            theme: Some("light".to_string()),
        },
        Icon {
            path: "icon-16-dark.png".into(),
            size: "16x16".to_string(),
            theme: Some("dark".to_string()),
        },
    ];
    spec.screenshots = vec!["shot.png".into()];
    spec.tools = vec![("whoami".to_string(), "Who the caller is.".to_string())];
    spec.tools_generated = true;
    spec.prompts = vec![Prompt {
        name: "review".to_string(),
        description: Some("Review a file.".to_string()),
        arguments: vec!["path".to_string()],
        text: "Please review ${arguments.path}.".to_string(),
    }];
    spec.prompts_generated = true;
    spec.localization = Some(Localization {
        default_locale: "en-US".to_string(),
        locales: BTreeMap::from([("de-DE".to_string(), "de-DE.json".into())]),
    });
    spec.user_config = vec![
        UserConfig {
            required: true,
            ..UserConfig::directory("project_dir", "Project directory", "Where it works.")
        },
        UserConfig {
            sensitive: true,
            ..UserConfig::string("api_key", "API key", "The key the server sends.")
        },
        UserConfig {
            multiple: true,
            default: Some(json!(["a", "b"])),
            ..UserConfig::string("tags", "Tags", "What it is about.")
        },
        UserConfig {
            min: Some(1.0),
            max: Some(100.0),
            default: Some(json!(10)),
            ..UserConfig::number("limit", "Limit", "How many results.")
        },
        UserConfig {
            default: Some(json!(true)),
            ..UserConfig::boolean("read_only", "Read only", "Never write.")
        },
        UserConfig::file("config_file", "Config file", "The config to load."),
    ];
    spec.args = vec![
        "--verbose".to_string(),
        "${user_config.project_dir}".to_string(),
    ];
    spec.env = BTreeMap::from([("API_KEY".to_string(), "${user_config.api_key}".to_string())]);
    spec.meta = json!({ "com.microsoft.windows": { "package_family_name": "Mcpb" } })
        .as_object()
        .cloned()
        .expect("an object");

    assert_fits(&spec);
}

#[test]
fn a_bundle_of_several_platforms_fits_the_schema() {
    let mut spec = minimal();
    spec.executables.insert(
        Platform::Darwin,
        Executable::new("server-binary-darwin", "arm64"),
    );
    spec.executables
        .insert(Platform::Win32, Executable::new("server-binary.exe", "x64"));
    spec.platform_overrides.insert(
        Platform::Win32,
        PlatformConfig {
            args: Some(vec!["--windows".to_string()]),
            ..PlatformConfig::default()
        },
    );

    assert_fits(&spec);
    assert_eq!(
        spec.manifest()["compatibility"]["platforms"],
        json!(["darwin", "linux", "win32"])
    );
}

#[test]
fn a_bundle_without_a_directory_prompt_fits_the_schema() {
    let mut spec = minimal();
    spec.user_config.clear();
    assert_fits(&spec);
}

/// If the schema did not load, or the validator stopped reporting, every test
/// above would pass without checking anything.
#[test]
fn the_schema_refuses_what_clients_would() {
    let mut manifest = minimal().manifest();
    manifest["server"] = json!({ "type": "carrier-pigeon" });
    assert!(!problems(&manifest).is_empty());

    let mut manifest = minimal().manifest();
    manifest
        .as_object_mut()
        .expect("an object")
        .remove("version");
    assert!(!problems(&manifest).is_empty());
}
