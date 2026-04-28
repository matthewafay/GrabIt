use std::path::PathBuf;

fn main() {
    #[cfg(target_os = "windows")]
    windows_build();
}

#[cfg(target_os = "windows")]
fn windows_build() {
    println!("cargo:rerun-if-changed=assets/manifest.xml");
    println!("cargo:rerun-if-changed=assets/icons/grabit.png");
    println!("cargo:rerun-if-changed=build.rs");
    // The generated `.rc` interpolates CARGO_PKG_VERSION; rebuild whenever
    // Cargo bumps the package version so VERSIONINFO never drifts from
    // Cargo.toml.
    println!("cargo:rerun-if-env-changed=CARGO_PKG_VERSION");

    generate_ico_from_png();
    let rc_path = generate_versioned_rc();
    embed_resource::compile(&rc_path, embed_resource::NONE);
}

/// Decode `assets/icons/grabit.png` and write a multi-size `grabit.ico`
/// alongside it. Windows picks the closest size at display time, so we
/// supply a useful ladder: 16/24/32/48/64/128/256.
#[cfg(target_os = "windows")]
fn generate_ico_from_png() {
    use image::imageops::FilterType;

    let png_path = PathBuf::from("assets/icons/grabit.png");
    let ico_path = PathBuf::from("assets/icons/grabit.ico");

    let src = match image::open(&png_path) {
        Ok(i) => i.to_rgba8(),
        Err(e) => {
            println!(
                "cargo:warning=could not read {}: {e} — shipping without an embedded icon",
                png_path.display()
            );
            return;
        }
    };

    let sizes = [16u32, 24, 32, 48, 64, 128, 256];
    let mut dir = ico::IconDir::new(ico::ResourceType::Icon);
    for size in sizes {
        let resized = image::imageops::resize(&src, size, size, FilterType::Lanczos3);
        let (w, h) = resized.dimensions();
        let icon_img = ico::IconImage::from_rgba_data(w, h, resized.into_raw());
        match ico::IconDirEntry::encode(&icon_img) {
            Ok(entry) => dir.add_entry(entry),
            Err(e) => println!("cargo:warning=skip ico size {size}: {e}"),
        }
    }

    let file = match std::fs::File::create(&ico_path) {
        Ok(f) => f,
        Err(e) => {
            println!("cargo:warning=cannot create {}: {e}", ico_path.display());
            return;
        }
    };
    if let Err(e) = dir.write(file) {
        println!("cargo:warning=cannot write ico: {e}");
    }
}

/// Generate `grabit.rc` into `OUT_DIR` with `VS_VERSION_INFO` strings
/// pulled from Cargo metadata. The icon and manifest are referenced via
/// absolute paths so the resource compiler doesn't need a particular
/// working directory.
///
/// Why bother: Windows Defender's heuristic ML scanner penalises
/// unsigned exes that lack rich version metadata. Filling out
/// `CompanyName`, `FileDescription`, `LegalCopyright`, etc. — with
/// values that match the binary's actual identity — pushes the
/// reputation score in our favour without touching code-signing.
#[cfg(target_os = "windows")]
fn generate_versioned_rc() -> PathBuf {
    let manifest_dir = PathBuf::from(
        std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR set by cargo"),
    );
    let out_dir =
        PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR set by cargo"));
    let pkg_version =
        std::env::var("CARGO_PKG_VERSION").expect("CARGO_PKG_VERSION set by cargo");

    // Cargo versions are dotted three-part (e.g. "1.4.0"); VERSIONINFO
    // wants four u16s (major, minor, build, revision). Pad the missing
    // tail with zeros and tolerate non-numeric tokens (pre-release suffix
    // like "-rc1") by treating them as 0 — a stable revision is more
    // important here than expressing a pre-release in the resource.
    let parts: Vec<u32> = pkg_version
        .split(|c: char| c == '.' || c == '-' || c == '+')
        .map(|tok| tok.parse::<u32>().unwrap_or(0))
        .chain(std::iter::repeat(0))
        .take(4)
        .collect();
    let comma_version = format!("{},{},{},{}", parts[0], parts[1], parts[2], parts[3]);
    let dot_version =
        format!("{}.{}.{}.{}", parts[0], parts[1], parts[2], parts[3]);

    let manifest_path = manifest_dir.join("assets").join("manifest.xml");
    let icon_path = manifest_dir.join("assets").join("icons").join("grabit.ico");

    // .rc string literals use C-style escaping: `\\` for a literal
    // backslash. Windows paths are full of backslashes, so escape them
    // here rather than emit raw paths the resource compiler would
    // misread.
    let manifest_str = manifest_path.display().to_string().replace('\\', "\\\\");
    let icon_str = icon_path.display().to_string().replace('\\', "\\\\");

    // `#pragma code_page(65001)` tells rc.exe the source bytes are UTF-8;
    // without it the compiler reads the file as the system codepage
    // (Windows-1252 on most en-US installs) and any non-ASCII character
    // we emit (em-dash, ©, etc.) ends up mojibake'd in the embedded
    // version strings.
    let rc = format!(
        r#"#pragma code_page(65001)
#include <winver.h>

1 24 "{manifest}"

1 ICON "{icon}"

VS_VERSION_INFO VERSIONINFO
FILEVERSION     {comma_version}
PRODUCTVERSION  {comma_version}
FILEFLAGSMASK   0x3fL
FILEFLAGS       0x0L
FILEOS          0x40004L
FILETYPE        0x1L
FILESUBTYPE     0x0L
BEGIN
    BLOCK "StringFileInfo"
    BEGIN
        BLOCK "040904b0"
        BEGIN
            VALUE "CompanyName",      "Matthew Fay"
            VALUE "FileDescription",  "GrabIt — Windows screenshot and screencast tool"
            VALUE "FileVersion",      "{dot_version}"
            VALUE "InternalName",     "grabit"
            VALUE "LegalCopyright",   "Copyright \251 2026 Matthew Fay. Released under source-available terms."
            VALUE "OriginalFilename", "grabit.exe"
            VALUE "ProductName",      "GrabIt"
            VALUE "ProductVersion",   "{dot_version}"
            VALUE "Comments",         "https://github.com/matthewafay/GrabIt"
        END
    END
    BLOCK "VarFileInfo"
    BEGIN
        VALUE "Translation", 0x409, 1200
    END
END
"#,
        manifest = manifest_str,
        icon = icon_str,
        comma_version = comma_version,
        dot_version = dot_version,
    );

    let rc_path = out_dir.join("grabit.rc");
    std::fs::write(&rc_path, rc).expect("write generated grabit.rc");
    rc_path
}
