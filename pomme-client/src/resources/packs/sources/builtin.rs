//! Bootstrap, explicit Minecraft installation and optional mapped theme.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{self, BufReader, Read};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use pomme_protocol::KnownPack;
use serde::Deserialize;
use serde_json::{Map, Value, json};

use super::{RepositorySource, path_error};
use crate::chat_component::Component;
use crate::resources::pack_resources::{
    EmbeddedResourcesSupplier, FileResourcesSupplier, MappedResourcesSupplier, PackType,
    VanillaResourcesSupplier,
};
use crate::resources::packs::format::{MAX_COMPONENT, format_component};
use crate::resources::packs::metadata::{PACK_SECTION, last_pre_minor_format};
use crate::resources::packs::{
    Pack, PackCompat, PackEnablementConfig, PackFormat, PackId, PackLocationInfo, PackMetadata,
    PackPosition,
};
use crate::resources::{ResourceError, ResourceId};

#[derive(PartialEq, Eq)]
struct BedrockUi {
    root: PathBuf,
    aliases: BTreeMap<ResourceId, PathBuf>,
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) enum ThemeSpecification {
    Pomme { root: PathBuf },
    Bedrock { root: PathBuf },
}

/// Pure mount definition for the built-in repository source.
struct ThemeMount<'a> {
    root: &'a std::path::Path,
    id: &'static str,
    name: &'static str,
    namespace: &'static str,
    remappings: BTreeMap<ResourceId, PathBuf>,
}

impl ThemeSpecification {
    fn mount(&self) -> Result<ThemeMount<'_>, ResourceError> {
        let (root, id, name, directory) = match self {
            Self::Pomme { root } => (root, "theme/pomme", "Pemozone theme assets", "panoramas"),
            Self::Bedrock { root } => {
                (root, "theme/bedrock", "Bedrock theme assets", "textures/ui")
            }
        };
        let remappings = (0..6)
            .map(|face| {
                Ok((
                    panorama_id(face)?,
                    // `/` on every platform: mapped targets reject `\`.
                    PathBuf::from(format!("{directory}/panorama_{face}.png")),
                ))
            })
            .collect::<Result<BTreeMap<_, _>, ResourceError>>()?;
        Ok(ThemeMount {
            root,
            id,
            name,
            namespace: "minecraft",
            remappings,
        })
    }
}

const PANORAMA_PATH_PREFIX: &str = "textures/gui/title/background/panorama_";

fn panorama_id(face: u32) -> Result<ResourceId, ResourceError> {
    ResourceId::from_parts("minecraft", &format!("{PANORAMA_PATH_PREFIX}{face}.png"))
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct InstallationLocation {
    pub version: String,
    /// The jar's `version.json`, extracted beside its assets.
    pub version_metadata: PathBuf,
    /// Parent of the extracted `assets` directory.
    pub extracted_root: PathBuf,
    pub index_file: PathBuf,
    pub objects_dir: PathBuf,
}

struct Installation {
    location: InstallationLocation,
    id: PackId,
    verified_format: Option<PackFormat>,
}

struct BuiltinState {
    pomme_id: PackId,
    installation: Option<Installation>,
    theme: Option<(ThemeSpecification, PackId)>,
    bedrock_ui: Option<(BedrockUi, PackId)>,
    target: Option<PackFormat>,
    /// Registration identity of each discovered built-in optional pack, keyed
    /// by its asset-index key, so a reload reuses it and the selection holds.
    optional_packs: BTreeMap<String, PackId>,
}

/// Box a clone; keep the concrete handle for subsequent installation/theme
/// changes. Only immutable owned supplier definitions escape a load's borrow.
#[derive(Clone)]
pub(crate) struct BuiltinRepositorySource {
    state: Rc<RefCell<BuiltinState>>,
}

impl BuiltinRepositorySource {
    /// I/O-free bootstrap: Pomme needs neither an installation nor a theme.
    pub(crate) fn new(target: Option<PackFormat>) -> Self {
        Self {
            state: Rc::new(RefCell::new(BuiltinState {
                pomme_id: PackId::allocate(),
                installation: None,
                theme: None,
                bedrock_ui: None,
                target,
                optional_packs: BTreeMap::new(),
            })),
        }
    }

    /// Reads only installed version.json on this explicit update, even for an
    /// unchanged location. An unverified version is never advertised as core.
    /// Callers must propagate target_format() to folder/registered handles and
    /// reload the repository; there is deliberately no cross-source controller.
    pub(crate) fn set_installation(&self, location: Option<InstallationLocation>) {
        let format = location.as_ref().and_then(|location| {
            match read_installed_pack_format(&location.version_metadata, &location.version) {
                Ok(format) => Some(format),
                Err(error) => {
                    tracing::warn!(%error, "Installed pack format unavailable; compatibility is unknown");
                    None
                }
            }
        });
        let mut state = self.state.borrow_mut();
        state.installation = location.map(|location| {
            let id = state
                .installation
                .as_ref()
                .filter(|old| old.location == location)
                .map_or_else(PackId::allocate, |old| old.id.clone());
            Installation {
                location,
                id,
                verified_format: format,
            }
        });
        state.target = format;
    }

    pub(crate) fn target_format(&self) -> Option<PackFormat> {
        self.state.borrow().target
    }

    pub(crate) fn set_theme(&self, theme: Option<ThemeSpecification>) {
        let mut state = self.state.borrow_mut();
        state.theme = theme.map(|theme| {
            let id = state
                .theme
                .as_ref()
                .filter(|(old, _)| old == &theme)
                .map_or_else(PackId::allocate, |(_, id)| id.clone());
            (theme, id)
        });
    }

    /// The menu preloads these sprites regardless of the selected panorama.
    /// A separate namespace keeps Bedrock's title out of Java's asset paths;
    /// `aliases` name the files whose capitals no id can spell.
    pub(crate) fn set_bedrock_ui(&self, root: PathBuf, aliases: BTreeMap<ResourceId, PathBuf>) {
        let mut state = self.state.borrow_mut();
        let ui = BedrockUi { root, aliases };
        let id = state
            .bedrock_ui
            .as_ref()
            .filter(|(old, _)| old == &ui)
            .map_or_else(PackId::allocate, |(_, id)| id.clone());
        state.bedrock_ui = Some((ui, id));
    }
}

impl RepositorySource for BuiltinRepositorySource {
    /// The embedded pack always loads. An installation or theme that can't be
    /// read is logged and left out, as vanilla's `FixedPathPackResources`
    /// builder skips roots it can't use.
    fn load_packs(&mut self) -> io::Result<Vec<Pack>> {
        let mut state = self.state.borrow_mut();
        let mut packs = vec![Pack::with_id(
            state.pomme_id.clone(),
            builtin_location("pomme", Component::text("Pemozone embedded assets")),
            // Embedded resources ship with this client, not an external format.
            computed_metadata("Pemozone embedded assets", PackCompat::Compatible),
            required(PackPosition::Bottom, true),
            Rc::new(EmbeddedResourcesSupplier::new()),
        )];
        if let Some(installation) = &state.installation {
            let input = &installation.location;
            let load = || -> io::Result<Pack> {
                let assets = input.extracted_root.join("assets");
                std::fs::read_dir(&assets).map_err(|error| {
                    path_error("open installed extracted assets", &assets, error)
                })?;
                let resources = Rc::new(VanillaResourcesSupplier::new(
                    input.extracted_root.clone(),
                    &input.index_file,
                    &input.objects_dir,
                    vanilla_metadata(installation.verified_format),
                ));
                let mut location = builtin_location(
                    "vanilla",
                    Component::translate("resourcePack.vanilla.name", Vec::new()),
                );
                location.known_pack_info = installation.verified_format.map(|_| KnownPack {
                    namespace: "minecraft".to_owned(),
                    id: "core".to_owned(),
                    version: input.version.clone(),
                });
                let metadata = Pack::read_metadata(
                    &location,
                    resources.as_ref(),
                    PackType::ClientResources,
                    state.target,
                )?
                .ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        "Minecraft pack metadata missing despite built-in fallback",
                    )
                })?;
                Ok(Pack::with_id(
                    installation.id.clone(),
                    location,
                    metadata,
                    // `ClientPackSource.VANILLA_SELECTION_CONFIG`.
                    required(PackPosition::Bottom, false),
                    resources,
                ))
            };
            match load() {
                Ok(pack) => packs.push(pack),
                Err(error) => tracing::warn!(
                    version = %input.version,
                    path = %input.extracted_root.display(),
                    %error,
                    "Failed to load the Minecraft installation's resources"
                ),
            }
        }
        if let Some((BedrockUi { root, aliases }, id)) = &state.bedrock_ui
            && root.is_dir()
        {
            match MappedResourcesSupplier::new(root.clone(), "bedrock".to_owned(), aliases.clone())
            {
                Ok(resources) => packs.push(Pack::with_id(
                    id.clone(),
                    builtin_location("pomme/bedrock-ui", Component::text("Bedrock UI assets")),
                    computed_metadata("Bedrock UI assets", PackCompat::Unknown),
                    required(PackPosition::Bottom, false),
                    Rc::new(resources),
                )),
                Err(error) => tracing::warn!(
                    path = %root.display(),
                    %error,
                    "Failed to load Bedrock UI resources"
                ),
            }
        }
        // Vanilla `ClientPackSource` also offers the index's
        // `minecraft:resourcepacks/*` zips (programmer_art, high_contrast) as
        // optional built-in packs.
        let optional_source = state
            .installation
            .as_ref()
            .map(|installation| installation.location.clone());
        if let Some(input) = optional_source {
            let target = state.target;
            for (key, path) in indexed_optional_pack_zips(&input.index_file, &input.objects_dir) {
                let optional_id = state
                    .optional_packs
                    .get(&key)
                    .cloned()
                    .unwrap_or_else(PackId::allocate);
                let load = || -> io::Result<Pack> {
                    let resources = Rc::new(FileResourcesSupplier::new(path.clone()));
                    let name = key
                        .strip_prefix(OPTIONAL_PACK_KEY_PREFIX)
                        .and_then(|name| name.strip_suffix(".zip"))
                        .unwrap_or(&key)
                        .to_owned();
                    let location = builtin_location(
                        &name,
                        Component::translate(format!("resourcePack.{name}.name"), Vec::new()),
                    );
                    let metadata = Pack::read_metadata(
                        &location,
                        resources.as_ref(),
                        PackType::ClientResources,
                        target,
                    )?
                    .ok_or_else(|| {
                        io::Error::new(
                            io::ErrorKind::InvalidData,
                            "Built-in optional pack metadata missing",
                        )
                    })?;
                    Ok(Pack::with_id(
                        optional_id.clone(),
                        location,
                        metadata,
                        // `ClientPackSource.BUILT_IN_SELECTION_CONFIG`.
                        PackEnablementConfig::DISCOVERED,
                        resources,
                    ))
                };
                match load() {
                    Ok(pack) => {
                        state.optional_packs.insert(key, optional_id);
                        packs.push(pack);
                    }
                    Err(error) => tracing::warn!(
                        path = %path.display(),
                        %error,
                        "Failed to load a built-in optional pack"
                    ),
                }
            }
        }
        if let Some((theme, id)) = &state.theme {
            match load_theme(theme, id) {
                Ok(Some(pack)) => packs.push(pack),
                Ok(None) => {}
                Err(error) => tracing::warn!(%error, "Failed to load theme resources"),
            }
        }
        Ok(packs)
    }
}

/// `None` until all six of the theme's faces are downloaded, so its panorama
/// never mixes with the faces of the packs below; the definition keeps its
/// ID, since later availability isn't a new theme.
fn load_theme(theme: &ThemeSpecification, id: &PackId) -> io::Result<Option<Pack>> {
    let mount = theme
        .mount()
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    if !mount
        .remappings
        .values()
        .all(|face| mount.root.join(face).is_file())
    {
        return Ok(None);
    }
    let resources = MappedResourcesSupplier::aliases_only(
        mount.root.to_owned(),
        mount.namespace.to_owned(),
        mount.remappings,
    )
    .map_err(|error| path_error("prepare theme mapping", mount.root, error))?;
    Ok(Some(Pack::with_id(
        id.clone(),
        builtin_location(mount.id, Component::text(mount.name)),
        computed_metadata(mount.name, PackCompat::Unknown),
        required(PackPosition::Top, false),
        Rc::new(resources),
    )))
}

fn builtin_location(id: &str, title: Component) -> PackLocationInfo {
    PackLocationInfo {
        id: id.to_owned(),
        title,
        known_pack_info: None,
    }
}

/// The Pomme-only embedded pack is fixed at the bottom, below vanilla, which
/// inserts just above it. The theme goes in at the top, so it lands above
/// vanilla however the enabled list got there, including after a theme switch.
fn required(default_position: PackPosition, fixed_position: bool) -> PackEnablementConfig {
    PackEnablementConfig {
        required: true,
        default_position,
        fixed_position,
    }
}

fn computed_metadata(description: &str, compatibility: PackCompat) -> PackMetadata {
    PackMetadata {
        description: Component::text(description),
        compatibility,
        overlays: Vec::new(),
    }
}

/// Asset-index keys of vanilla's optional built-in packs, under
/// `minecraft/resourcepacks/<name>.zip`. Vanilla's `ClientPackSource` takes the
/// pack id from the file name and titles it `resourcePack.<name>.name`; Pomme
/// reads both from the index key.
const OPTIONAL_PACK_KEY_PREFIX: &str = "minecraft/resourcepacks/";

/// The index's `minecraft/resourcepacks/*.zip` objects, each an optional
/// built-in pack backed by the shared objects directory. A missing or
/// unreadable index contributes none, as vanilla's `IndexedAssetSource` logs
/// and continues.
fn indexed_optional_pack_zips(index_file: &Path, objects_dir: &Path) -> Vec<(String, PathBuf)> {
    #[derive(Deserialize)]
    struct Index {
        #[serde(default)]
        objects: Map<String, Value>,
    }

    let index = File::open(index_file).and_then(|file| {
        serde_json::from_reader::<_, Index>(BufReader::new(file)).map_err(io::Error::other)
    });
    let index = match index {
        Ok(index) => index,
        Err(error) => {
            tracing::warn!(path = %index_file.display(), %error, "Can't read the asset index for built-in packs");
            return Vec::new();
        }
    };
    index
        .objects
        .into_iter()
        .filter_map(|(key, object)| {
            let name = key.strip_prefix(OPTIONAL_PACK_KEY_PREFIX)?;
            if !name.ends_with(".zip") {
                return None;
            }
            let hash = object
                .get("hash")
                .and_then(Value::as_str)
                .filter(|hash| hash.len() == 40 && hash.bytes().all(|b| b.is_ascii_hexdigit()))?;
            Some((key, objects_dir.join(&hash[..2]).join(hash)))
        })
        .collect()
}

fn vanilla_metadata(format: Option<PackFormat>) -> Map<String, Value> {
    let mut pack = Map::new();
    pack.insert(
        "description".to_owned(),
        json!({"translate": "resourcePack.vanilla.description"}),
    );
    if let Some(format) = format {
        if format.major() < 15 {
            // Modern transition ranges require pack_format >= 15. Standalone
            // legacy declarations encode only minor 0, as read from the legacy
            // version.json resource field; nonzero minors are not representable.
            pack.insert("pack_format".to_owned(), json!(format.major()));
        } else {
            // Vanilla PackFormat.minorRange: [this, (major, Integer.MAX_VALUE)].
            pack.insert(
                "min_format".to_owned(),
                json!([format.major(), format.minor()]),
            );
            pack.insert(
                "max_format".to_owned(),
                json!([format.major(), MAX_COMPONENT]),
            );
            if format.major() <= last_pre_minor_format(PackType::ClientResources) {
                pack.insert("pack_format".to_owned(), json!(format.major()));
                pack.insert("supported_formats".to_owned(), json!(format.major()));
            }
        }
    }
    // Unknown installation: description-only fallback, no invented format.
    // The strict factory still validates present external metadata and overlays.
    Map::from_iter([(PACK_SECTION.to_owned(), Value::Object(pack))])
}

// Installed version.json, distinct from selected-pack metadata.
const VERSION_RESOURCE_FORMAT: &str = "resource";
const VERSION_RESOURCE_MAJOR: &str = "resource_major";
const VERSION_RESOURCE_MINOR: &str = "resource_minor";

// Same startup bound as the installer's readiness check, without coupling APIs.
const MAX_VERSION_JSON_SIZE: u64 = 1024 * 1024;

#[derive(Debug, thiserror::Error)]
#[error("cannot read resource pack format for {version} from {path}: {source}")]
struct InstalledFormatError {
    path: PathBuf,
    version: String,
    #[source]
    source: FormatReadError,
}

#[derive(Debug, thiserror::Error)]
enum FormatReadError {
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
    #[error("invalid version.json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("version.json exceeds the {MAX_VERSION_JSON_SIZE}-byte limit")]
    TooLarge,
    #[error("version.json identifies a different version: {0}")]
    VersionMismatch(String),
    #[error("invalid version.json pack_version: {0}")]
    InvalidFormat(&'static str),
}

/// Read the installed version.json, never selected packs or protocol metadata.
/// Missing and corrupt metadata are both explicit errors, which leave the
/// format unknown.
fn read_installed_pack_format(
    path: &Path,
    expected_version: &str,
) -> Result<PackFormat, InstalledFormatError> {
    read_format(path, expected_version).map_err(|source| InstalledFormatError {
        path: path.to_owned(),
        version: expected_version.to_owned(),
        source,
    })
}

fn read_format(path: &Path, expected_version: &str) -> Result<PackFormat, FormatReadError> {
    #[derive(Deserialize)]
    struct JarVersion {
        id: String,
        pack_version: Value,
    }

    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(MAX_VERSION_JSON_SIZE + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_VERSION_JSON_SIZE {
        return Err(FormatReadError::TooLarge);
    }
    let metadata: JarVersion = serde_json::from_slice(&bytes)?;
    if metadata.id != expected_version {
        return Err(FormatReadError::VersionMismatch(metadata.id));
    }
    let version = &metadata.pack_version;
    let component = |key| {
        version
            .get(key)
            .and_then(format_component)
            .ok_or(FormatReadError::InvalidFormat(key))
    };
    let (major, minor) = if version.get(VERSION_RESOURCE_MAJOR).is_some()
        || version.get(VERSION_RESOURCE_MINOR).is_some()
    {
        (
            component(VERSION_RESOURCE_MAJOR)?,
            component(VERSION_RESOURCE_MINOR)?,
        )
    } else {
        (component(VERSION_RESOURCE_FORMAT)?, 0)
    };
    PackFormat::new(major, minor).ok_or(FormatReadError::InvalidFormat("resource components"))
}

#[cfg(test)]
mod repository_source_tests {
    use super::super::{RepositorySource, tests};
    use super::{
        BuiltinRepositorySource, MappedResourcesSupplier, PackFormat, ThemeSpecification,
        read_installed_pack_format,
    };

    #[test]
    fn test_load_packs() {
        let mut source = BuiltinRepositorySource::new(None);
        tests::test_load_packs(&mut source, &["pomme"]);
    }

    #[test]
    fn installed_format_reads_either_pack_version_layout() {
        let path =
            std::env::temp_dir().join(format!("pomme-version-json-{}.json", std::process::id()));
        for (pack_version, (major, minor)) in [
            (r#"{"resource": 34}"#, (34, 0)),
            (r#"{"resource_major": 75, "resource_minor": 1}"#, (75, 1)),
        ] {
            let json = format!(r#"{{"id": "26.2", "pack_version": {pack_version}}}"#);
            std::fs::write(&path, json).unwrap();
            assert_eq!(
                read_installed_pack_format(&path, "26.2").unwrap(),
                PackFormat::new(major, minor).unwrap()
            );
        }
        assert!(read_installed_pack_format(&path, "26.1").is_err());
        std::fs::remove_file(&path).unwrap();
        assert!(read_installed_pack_format(&path, "26.2").is_err());
    }

    #[test]
    fn every_theme_mount_prepares_its_mapping() {
        let root = std::env::temp_dir();
        for theme in [
            ThemeSpecification::Pomme { root: root.clone() },
            ThemeSpecification::Bedrock { root: root.clone() },
        ] {
            let mount = theme.mount().unwrap();
            MappedResourcesSupplier::new(
                mount.root.to_owned(),
                mount.namespace.to_owned(),
                mount.remappings,
            )
            .unwrap_or_else(|error| panic!("{}: {error}", mount.id));
        }
    }

    #[test]
    fn a_theme_loads_only_with_all_six_faces() {
        let temp = crate::resources::test_dir::TempDir::new().unwrap();
        let faces = temp.0.join("panoramas");
        std::fs::create_dir_all(&faces).unwrap();
        let theme = ThemeSpecification::Pomme {
            root: temp.0.clone(),
        };
        let id = crate::resources::packs::PackId::allocate();
        for face in 0..5 {
            std::fs::write(faces.join(format!("panorama_{face}.png")), b"face").unwrap();
        }
        assert!(super::load_theme(&theme, &id).unwrap().is_none());
        std::fs::write(faces.join("panorama_5.png"), b"face").unwrap();
        assert!(super::load_theme(&theme, &id).unwrap().is_some());
    }

    #[test]
    fn indexed_optional_packs_are_discovered_from_the_index() {
        let temp = crate::resources::test_dir::TempDir::new().unwrap();
        let mut source = optional_fixture(&temp);
        tests::test_load_packs(
            &mut source,
            &["pomme", "vanilla", "high_contrast", "programmer_art"],
        );
    }

    #[test]
    fn optional_pack_identity_survives_reloads() {
        let temp = crate::resources::test_dir::TempDir::new().unwrap();
        let mut source = optional_fixture(&temp);
        let id_of = |source: &mut BuiltinRepositorySource| {
            source
                .load_packs()
                .unwrap()
                .into_iter()
                .find(|pack| pack.location().id == "programmer_art")
                .unwrap()
                .id()
                .clone()
        };
        assert_eq!(
            id_of(&mut source),
            id_of(&mut source),
            "reload reuses the retained registration"
        );
    }

    #[test]
    fn optional_packs_require_an_installation() {
        // A source with no installation advertises only the embedded pack.
        let mut bare = BuiltinRepositorySource::new(None);
        tests::test_load_packs(&mut bare, &["pomme"]);
    }

    #[test]
    fn optional_pack_without_metadata_is_skipped() {
        let temp = crate::resources::test_dir::TempDir::new().unwrap();
        let objects = temp.0.join("objects");
        std::fs::create_dir_all(&objects).unwrap();
        let hash = "aa".repeat(20);
        let object = objects.join(&hash[..2]).join(&hash);
        std::fs::create_dir_all(object.parent().unwrap()).unwrap();
        std::fs::write(&object, b"not a zip").unwrap();
        let index = temp.0.join("index.json");
        std::fs::write(
            &index,
            format!(r#"{{"objects":{{"minecraft/resourcepacks/broken.zip":{{"hash":"{hash}","size":10}}}}}}"#),
        )
        .unwrap();
        let mut source = BuiltinRepositorySource::new(None);
        source.set_installation(Some(installation(&temp, &index, &objects)));
        tests::test_load_packs(&mut source, &["pomme", "vanilla"]);
    }

    fn installation(
        temp: &crate::resources::test_dir::TempDir,
        index: &std::path::Path,
        objects: &std::path::Path,
    ) -> super::InstallationLocation {
        let extracted_root = temp.0.join("extracted");
        std::fs::create_dir_all(extracted_root.join("assets")).unwrap();
        let version_metadata = temp.0.join("version.json");
        std::fs::write(
            &version_metadata,
            r#"{"id":"26.2","pack_version":{"resource_major":75,"resource_minor":0}}"#,
        )
        .unwrap();
        super::InstallationLocation {
            version: "26.2".to_owned(),
            version_metadata,
            extracted_root,
            index_file: index.to_owned(),
            objects_dir: objects.to_owned(),
        }
    }

    /// A source with one installation exposing two index zips, each a real
    /// resource pack with its own `pack.mcmeta`.
    fn optional_fixture(temp: &crate::resources::test_dir::TempDir) -> BuiltinRepositorySource {
        use std::io::Write;

        use zip::ZipWriter;
        use zip::write::SimpleFileOptions;

        let objects = temp.0.join("objects");
        std::fs::create_dir_all(&objects).unwrap();
        let mut entries = Vec::new();
        for (index, name) in ["programmer_art", "high_contrast"].into_iter().enumerate() {
            let mut archive = ZipWriter::new(std::io::Cursor::new(Vec::new()));
            archive
                .start_file("pack.mcmeta", SimpleFileOptions::default())
                .unwrap();
            archive
                .write_all(br#"{"pack":{"pack_format":75,"description":"fixture"}}"#)
                .unwrap();
            let bytes = archive.finish().unwrap().into_inner();
            // Any distinct well-formed hash works: only the path shape is read.
            let hash = format!("{index:040x}");
            let object = objects.join(&hash[..2]).join(&hash);
            std::fs::create_dir_all(object.parent().unwrap()).unwrap();
            std::fs::write(&object, &bytes).unwrap();
            entries.push(format!(
                r#""minecraft/resourcepacks/{name}.zip":{{"hash":"{hash}","size":{}}}"#,
                bytes.len()
            ));
        }
        let index = temp.0.join("index.json");
        std::fs::write(
            &index,
            format!(r#"{{"objects":{{{}}}}}"#, entries.join(",")),
        )
        .unwrap();
        let source = BuiltinRepositorySource::new(None);
        source.set_installation(Some(installation(temp, &index, &objects)));
        source
    }
}
