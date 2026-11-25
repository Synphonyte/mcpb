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

/// The manifest of the spec's own `examples/calculator-rust`, as it is checked
/// in there; see `tests/fixtures/README.md`.
const REFERENCE: &str = include_str!("fixtures/calculator-rust.manifest.json");

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
        version: "1.2.3".parse().expect("a semantic version"),
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

/// A spec that sets every field the crate writes.
fn full() -> BundleSpec {
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

    spec
}

#[test]
fn a_spec_that_uses_every_field_fits_the_schema() {
    assert_fits(&full());
}

/// One bundle that runs on three platforms, each with a binary of its own.
fn several_platforms() -> BundleSpec {
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

    spec
}

#[test]
fn a_bundle_of_several_platforms_fits_the_schema() {
    let spec = several_platforms();
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

/// What the spec's own example bundle packs to, without the `$schema` its
/// checked-in manifest points at: this crate writes none.
fn reference_manifest() -> Value {
    let mut manifest: Value = serde_json::from_str(REFERENCE).expect("the fixture is JSON");
    manifest
        .as_object_mut()
        .expect("an object")
        .remove("$schema");
    manifest
}

#[test]
fn the_reference_manifest_is_one_we_could_have_written() {
    let reference = reference_manifest();
    assert_eq!(problems(&reference), Vec::<String>::new());

    let spec = BundleSpec::from_manifest(&reference).expect("the reference manifest reads");
    assert_eq!(
        spec.manifest(),
        reference,
        "what we write of the reference manifest is not the reference manifest"
    );
}

#[test]
fn the_reference_bundle_packs_and_reads_back_the_same() {
    let reference = reference_manifest();
    let dir = tempfile::TempDir::new().expect("a temporary directory");

    // The reference names one binary for both platforms, which both of them
    // start.
    let binary = dir.path().join("server").join("mcp-calculator");
    std::fs::create_dir_all(binary.parent().expect("a parent")).expect("the directory");
    std::fs::write(&binary, b"\x7fELF fake").expect("the binary");

    let mut spec = BundleSpec::from_manifest(&reference)
        .expect("the reference manifest reads")
        .in_dir(dir.path());
    assert_eq!(spec.file_name(), "calculator-rust-1.0.0-darwin-linux.mcpb");

    // "Same manifest, new version".
    spec.version = "1.0.1".parse().expect("a semantic version");
    let packed = dir.path().join(spec.file_name());
    spec.pack(&packed).expect("the reference bundle packs");

    let mut expected = reference.clone();
    expected["version"] = json!("1.0.1");
    let bundle = mcpb::Bundle::open(&packed).expect("the bundle we just wrote opens");
    assert_eq!(*bundle.manifest(), expected);
    assert_eq!(
        bundle
            .entries()
            .iter()
            .map(|entry| entry.name.as_str())
            .collect::<Vec<_>>(),
        vec!["manifest.json", "server/mcp-calculator"],
        "one binary serves both platforms"
    );

    let read = bundle.spec().expect("the bundle reads back as a spec");
    assert_eq!(read.manifest(), expected);
}

/// A manifest that reads back as the spec that wrote it, so a caller can take a
/// bundle, edit it and write it again without the manifest drifting.
#[track_caller]
fn assert_round_trips(spec: &BundleSpec) {
    let manifest = spec.manifest();
    let read = BundleSpec::from_manifest(&manifest)
        .unwrap_or_else(|error| panic!("{}'s manifest does not read: {error}", spec.name));
    assert_eq!(
        read.manifest(),
        manifest,
        "{}'s manifest changes when it is read and written again",
        spec.name
    );
}

#[test]
fn the_manifests_we_write_read_back_as_the_same() {
    for spec in [minimal(), full(), several_platforms()] {
        assert_round_trips(&spec);
    }
}

#[test]
fn a_spec_serializes_as_its_manifest() {
    let spec = full();
    assert_eq!(
        serde_json::to_value(&spec).expect("a spec serializes"),
        spec.manifest()
    );

    let json = serde_json::to_string(&spec).expect("a spec serializes");
    let read: BundleSpec = serde_json::from_str(&json).expect("a manifest reads as a spec");
    assert_eq!(read.manifest(), spec.manifest());
}

#[test]
fn a_manifest_that_is_not_one_says_what_is_wrong() {
    let mut manifest = minimal().manifest();
    manifest["compatibility"] = json!({ "platforms": ["plan9"] });
    let error = BundleSpec::from_manifest(&manifest).unwrap_err();
    assert!(
        error.to_string().contains("\"plan9\" is no platform"),
        "{error}"
    );

    let mut manifest = minimal().manifest();
    manifest["version"] = json!("1.2.3.4");
    let error = BundleSpec::from_manifest(&manifest).unwrap_err();
    assert!(error.to_string().contains("does not parse"), "{error}");

    let mut manifest = minimal().manifest();
    manifest["author"] = json!("just a name");
    let error = BundleSpec::from_manifest(&manifest).unwrap_err();
    assert!(
        error.to_string().contains("author is no JSON object"),
        "{error}"
    );

    let mut manifest = minimal().manifest();
    manifest
        .as_object_mut()
        .expect("an object")
        .remove("server");
    let error = BundleSpec::from_manifest(&manifest).unwrap_err();
    assert!(error.to_string().contains("there is no server"), "{error}");
}

#[test]
fn a_platform_is_serialized_as_the_manifest_spells_it() {
    for (platform, name) in [
        (Platform::Darwin, "darwin"),
        (Platform::Linux, "linux"),
        (Platform::Win32, "win32"),
    ] {
        assert_eq!(serde_json::to_value(platform).unwrap(), json!(name));
        assert_eq!(
            serde_json::from_value::<Platform>(json!(name)).unwrap(),
            platform
        );
    }

    assert!(serde_json::from_value::<Platform>(json!("macos")).is_err());
}
